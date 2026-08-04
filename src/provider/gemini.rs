//! Gemini：streamGenerateContent + SSE。
//! 兩個 Gemini 專屬的怪癖都關在這個檔案裡：
//! (1) tool schema 是受限的 OpenAPI 子集，要 sanitize
//! (2) 工具結果用「工具名」配對，不是用 id

use super::types::{Block, Message, Role, ToolDecl};
use super::{Decoder, Provider, emit};
use anyhow::Result;
use serde_json::{Value, json};

const DEFAULT_MODEL: &str = "gemini-2.5-flash";

pub struct Gemini {
    api_key: String,
    model: String,
}

impl Gemini {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            api_key: std::env::var("GEMINI_API_KEY")?,
            model: std::env::var("RS_AGENT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string()),
        })
    }
}

const UNSUPPORTED_KEYS: &[&str] = &[
    "$schema", "$id", "$comment", "$defs", "definitions", "$ref",
    "additionalProperties", "unevaluatedProperties", "patternProperties",
    "oneOf", "allOf", "not", "if", "then", "else", "const", "prefixItems",
];

/// Gemini 的 tool schema 是受限的 OpenAPI 子集，把不支援的 key 遞迴刪掉
fn sanitize_schema(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| !UNSUPPORTED_KEYS.contains(&k.as_str()))
                .map(|(k, val)| (k.clone(), sanitize_schema(val)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(sanitize_schema).collect()),
        _ => v.clone(),
    }
}

/// 中性訊息 → Gemini 的 contents
fn to_contents(history: &[Message]) -> Vec<Value> {
    history
        .iter()
        .filter_map(|m| {
            let parts: Vec<Value> = m
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text(t) => Some(json!({"text": t})),
                    Block::ToolCall { name, args, .. } => {
                        Some(json!({"functionCall": {"name": name, "args": args}}))
                    }
                    // Gemini 用工具名配對，id 用不到
                    Block::ToolResult { name, content, .. } => Some(json!({
                        "functionResponse": {"name": name, "response": {"content": content}}
                    })),
                    // 別家 provider 的專屬塊，跳過
                    Block::Opaque(_) => None,
                })
                .collect();
            if parts.is_empty() {
                return None;
            }
            let role = if m.role == Role::Assistant { "model" } else { "user" };
            Some(json!({"role": role, "parts": parts}))
        })
        .collect()
}

impl Provider for Gemini {
    fn name(&self) -> &str {
        "gemini"
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
                    "parameters": sanitize_schema(&t.schema),
                })
            })
            .collect();
        let body = json!({
            "systemInstruction": {"parts": [{"text": system}]},
            "contents": to_contents(history),
            "tools": [{"functionDeclarations": decls}],
        });
        Ok(client
            .post(format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?alt=sse",
                self.model
            ))
            .header("x-goog-api-key", &self.api_key)
            .json(&body))
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::new(GeminiDecoder::default())
    }
}

/// Gemini 的每個 chunk 都是完整的 JSON，工具呼叫也是完整物件——
/// 不需要像 OpenAI/Claude 那樣累積 partial JSON
#[derive(Default)]
struct GeminiDecoder {
    text: String,
    calls: Vec<Block>,
}

impl Decoder for GeminiDecoder {
    fn push(&mut self, data: &str) -> Result<()> {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        let Some(parts) = v["candidates"][0]["content"]["parts"].as_array() else {
            return Ok(());
        };
        for part in parts {
            if let Some(t) = part["text"].as_str() {
                emit(t);
                self.text.push_str(t);
            }
            if part["functionCall"].is_object() {
                let call = &part["functionCall"];
                self.calls.push(Block::ToolCall {
                    // Gemini 不給 id，補一個序號讓中性型別完整
                    id: format!("call_{}", self.calls.len()),
                    name: call["name"].as_str().unwrap_or_default().to_string(),
                    args: call["args"].clone(),
                });
            }
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Vec<Block> {
        let mut blocks = Vec::new();
        if !self.text.is_empty() {
            blocks.push(Block::Text(self.text));
        }
        blocks.extend(self.calls);
        blocks
    }
}
