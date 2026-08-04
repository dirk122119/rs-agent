//! OpenAI Chat Completions。Grok（xAI）也走這裡——它是 OpenAI-compatible，
//! 只換 base URL 與模型名，所以兩家共用同一份實作。Ollama 也是同一條路。
//!
//! 兩個和 Gemini 不同的地方：
//! (1) 工具參數在串流裡是**字串碎片**，要按 index 累積起來才能 parse
//! (2) 工具結果是獨立的 role="tool" 訊息，一個結果一則——
//!     所以一則中性訊息會展開成多則 wire 訊息

use super::types::{Block, Message, Role, ToolDecl};
use super::{Decoder, Provider, emit};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn model_or(default: &str) -> String {
    std::env::var("RS_AGENT_MODEL").unwrap_or_else(|_| default.to_string())
}

pub struct OpenAiCompat {
    name: &'static str,
    base: &'static str,
    api_key: String,
    model: String,
}

impl OpenAiCompat {
    pub fn openai() -> Result<Self> {
        Ok(Self {
            name: "openai",
            base: "https://api.openai.com/v1",
            api_key: std::env::var("OPENAI_API_KEY")?,
            model: model_or("gpt-4o"),
        })
    }

    pub fn grok() -> Result<Self> {
        Ok(Self {
            name: "grok",
            base: "https://api.x.ai/v1",
            api_key: std::env::var("XAI_API_KEY")?,
            model: model_or("grok-4"),
        })
    }
}

/// 中性訊息 → OpenAI 的 messages。system prompt 是第一則訊息（不是獨立欄位）
fn to_messages(history: &[Message], system: &str) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];
    for m in history {
        match m.role {
            Role::Assistant => {
                let mut text = String::new();
                let mut calls = Vec::new();
                for b in &m.blocks {
                    match b {
                        Block::Text(t) => text.push_str(t),
                        Block::ToolCall { id, name, args } => calls.push(json!({
                            "id": id,
                            "type": "function",
                            // arguments 是**字串**，不是物件
                            "function": {"name": name, "arguments": args.to_string()},
                        })),
                        _ => {}
                    }
                }
                let mut msg = json!({"role": "assistant"});
                msg["content"] = if text.is_empty() {
                    Value::Null
                } else {
                    Value::String(text)
                };
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                out.push(msg);
            }
            Role::User => {
                // 工具結果各自成為一則 role="tool" 訊息，用 id 配對
                let mut text = String::new();
                for b in &m.blocks {
                    match b {
                        Block::Text(t) => text.push_str(t),
                        Block::ToolResult { id, content, .. } => out.push(json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": content,
                        })),
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    out.push(json!({"role": "user", "content": text}));
                }
            }
        }
    }
    out
}

impl Provider for OpenAiCompat {
    fn name(&self) -> &str {
        self.name
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
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.schema,
                    }
                })
            })
            .collect();
        let body = json!({
            "model": self.model,
            "stream": true,
            "messages": to_messages(history, system),
            "tools": decls,
        });
        Ok(client
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.api_key)
            .json(&body))
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::new(CompatDecoder::default())
    }
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    json: String,
}

#[derive(Default)]
struct CompatDecoder {
    text: String,
    /// 工具呼叫按 delta 裡的 index 分組累積
    calls: BTreeMap<u64, PartialCall>,
}

impl Decoder for CompatDecoder {
    fn push(&mut self, data: &str) -> Result<()> {
        if data == "[DONE]" {
            return Ok(());
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        let delta = &v["choices"][0]["delta"];
        if let Some(t) = delta["content"].as_str() {
            emit(t);
            self.text.push_str(t);
        }
        if let Some(tcs) = delta["tool_calls"].as_array() {
            for tc in tcs {
                let idx = tc["index"].as_u64().unwrap_or(0);
                let slot = self.calls.entry(idx).or_default();
                // id 與 name 只在該 index 的第一個片段出現
                if let Some(id) = tc["id"].as_str() {
                    slot.id = id.to_string();
                }
                if let Some(n) = tc["function"]["name"].as_str() {
                    slot.name = n.to_string();
                }
                if let Some(a) = tc["function"]["arguments"].as_str() {
                    slot.json.push_str(a);
                }
            }
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Vec<Block> {
        let mut blocks = Vec::new();
        if !self.text.is_empty() {
            blocks.push(Block::Text(self.text));
        }
        for (_, c) in self.calls {
            blocks.push(Block::ToolCall {
                id: c.id,
                name: c.name,
                // 累積完才 parse；空字串當成沒有參數
                args: serde_json::from_str(&c.json).unwrap_or_else(|_| json!({})),
            });
        }
        blocks
    }
}
