//! Claude Messages API。三個和另外兩家不同的地方：
//! (1) system prompt 是**頂層欄位**，不是訊息也不是 systemInstruction
//! (2) max_tokens **必填**（另兩家選填）
//! (3) SSE 是具型別的事件流（content_block_start/delta/stop），
//!     工具參數和 Gemini 不同，是 partial JSON 碎片
//!
//! 還有一個容易踩的坑：claude-opus-5 預設開啟思考，而多輪對話**必須把
//! thinking block 原封不動送回**（含 signature），否則會 400。所以解碼器
//! 忠實重建整個 content 陣列，不認識的塊型別存成 Block::Opaque 原樣回送。

use super::types::{Block, Message, Role, ToolDecl};
use super::{Decoder, Provider, emit};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const DEFAULT_MODEL: &str = "claude-opus-5";
/// 串流時給足空間：max_tokens 同時蓋住思考與回覆文字
const MAX_TOKENS: u32 = 64000;

pub struct Claude {
    api_key: String,
    model: String,
}

impl Claude {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            api_key: std::env::var("ANTHROPIC_API_KEY")?,
            model: std::env::var("RS_AGENT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string()),
        })
    }
}

/// 中性訊息 → Claude 的 messages（system 不在這裡，它是頂層欄位）
fn to_messages(history: &[Message]) -> Vec<Value> {
    history
        .iter()
        .filter_map(|m| {
            let content: Vec<Value> = m
                .blocks
                .iter()
                .map(|b| match b {
                    Block::Text(t) => json!({"type": "text", "text": t}),
                    Block::ToolCall { id, name, args } => {
                        json!({"type": "tool_use", "id": id, "name": name, "input": args})
                    }
                    Block::ToolResult { id, content, .. } => {
                        json!({"type": "tool_result", "tool_use_id": id, "content": content})
                    }
                    // thinking 等塊：原封不動送回，改動會被 API 拒絕
                    Block::Opaque(v) => v.clone(),
                })
                .collect();
            if content.is_empty() {
                return None;
            }
            let role = if m.role == Role::Assistant { "assistant" } else { "user" };
            Some(json!({"role": role, "content": content}))
        })
        .collect()
}

impl Provider for Claude {
    fn name(&self) -> &str {
        "claude"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn request(
        &self,
        client: &reqwest::Client,
        history: &[Message],
        tools: &[ToolDecl],
        system: &str,
    ) -> Result<reqwest::RequestBuilder> {
        let decls: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    // Claude 叫 input_schema，不是 parameters
                    "input_schema": t.schema,
                })
            })
            .collect();
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "system": system,
            "tools": decls,
            "messages": to_messages(history),
            // 安全分類器擋下請求時，改由備援模型接手而不是直接失敗
            "fallbacks": "default",
        });
        Ok(client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "server-side-fallback-2026-07-01")
            .json(&body))
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::new(ClaudeDecoder::default())
    }
}

enum Slot {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
    /// thinking / redacted_thinking 等：保留原始物件，收尾時原樣回送
    Other(Value),
}

#[derive(Default)]
struct ClaudeDecoder {
    /// 按事件裡的 index 存放，BTreeMap 保證收尾時順序與原始一致
    slots: BTreeMap<u64, Slot>,
}

impl Decoder for ClaudeDecoder {
    fn push(&mut self, data: &str) -> Result<()> {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        let idx = v["index"].as_u64().unwrap_or(0);
        match v["type"].as_str().unwrap_or_default() {
            "content_block_start" => {
                let cb = &v["content_block"];
                let slot = match cb["type"].as_str().unwrap_or_default() {
                    "text" => Slot::Text(cb["text"].as_str().unwrap_or_default().to_string()),
                    "tool_use" => Slot::ToolUse {
                        id: cb["id"].as_str().unwrap_or_default().to_string(),
                        name: cb["name"].as_str().unwrap_or_default().to_string(),
                        json: String::new(),
                    },
                    _ => Slot::Other(cb.clone()),
                };
                self.slots.insert(idx, slot);
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match self.slots.get_mut(&idx) {
                    Some(Slot::Text(s)) => {
                        if let Some(t) = d["text"].as_str() {
                            emit(t);
                            s.push_str(t);
                        }
                    }
                    Some(Slot::ToolUse { json, .. }) => {
                        if let Some(p) = d["partial_json"].as_str() {
                            json.push_str(p);
                        }
                    }
                    Some(Slot::Other(obj)) => {
                        // thinking_delta / signature_delta：接回原欄位，其他忽略
                        for (delta_key, field) in
                            [("thinking", "thinking"), ("signature", "signature")]
                        {
                            if let Some(s) = d[delta_key].as_str() {
                                let cur = obj[field].as_str().unwrap_or_default();
                                obj[field] = Value::String(format!("{cur}{s}"));
                            }
                        }
                    }
                    None => {}
                }
            }
            "message_delta" => {
                // 安全分類器擋下時 content 會是空的或只有一半，講清楚比靜默好
                if v["delta"]["stop_reason"].as_str() == Some("refusal") {
                    emit("\n[模型因安全政策拒絕了這次請求]\n");
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Vec<Block> {
        self.slots
            .into_values()
            .map(|s| match s {
                Slot::Text(t) => Block::Text(t),
                Slot::ToolUse { id, name, json } => Block::ToolCall {
                    id,
                    name,
                    // 累積完才 parse；空字串當成沒有參數
                    args: serde_json::from_str(&json).unwrap_or_else(|_| json!({})),
                },
                Slot::Other(v) => Block::Opaque(v),
            })
            .collect()
    }
}
