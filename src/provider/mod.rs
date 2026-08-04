//! Provider 抽象：把「哪家模型」收斂成兩件事——怎麼組請求、怎麼解事件。
//!
//! 關鍵觀察是：HTTP 與 SSE 的串流迴圈各家完全一樣，只有請求組裝與事件解析不同。
//! 所以 trait 全部是同步方法（因此天然 dyn-compatible，不需要 async_trait），
//! 非同步的部分由共用的 stream_once 一次寫好。

mod claude;
mod gemini;
mod openai;
pub mod types;

pub use types::{Block, Message, ToolDecl, tool_calls};

use anyhow::{Result, bail};
use futures::StreamExt;
use std::io::{self, Write};

// ---------- SSE parser（同 Step 3，三家都是 data: 行） ----------

struct SseParser {
    buf: String,
}

impl SseParser {
    fn new() -> Self {
        Self { buf: String::new() }
    }

    /// 餵進一個 chunk，吐出所有「已完整」的 data payload
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf
            .push_str(&String::from_utf8_lossy(chunk).replace("\r\n", "\n"));
        let mut out = Vec::new();
        while let Some(pos) = self.buf.find("\n\n") {
            let event: String = self.buf.drain(..pos + 2).collect();
            for line in event.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    out.push(data.trim_start().to_string());
                }
            }
        }
        out
    }
}

// ---------- 兩個 trait ----------

pub trait Provider {
    /// banner 上顯示的名字
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    /// 組出一個「還沒送出」的請求：URL、認證 header、body 都在這裡決定
    fn request(
        &self,
        client: &reqwest::Client,
        history: &[Message],
        tools: &[ToolDecl],
        system: &str,
    ) -> Result<reqwest::RequestBuilder>;
    /// 每次請求開一個新的解碼器（它要保存跨事件的累積狀態）
    fn decoder(&self) -> Box<dyn Decoder>;
}

/// 把一連串 SSE data payload 解成這一輪的內容塊。
/// 各家的累積狀態差異很大（OpenAI 與 Claude 的工具參數是 partial JSON 片段），
/// 所以狀態由各自的解碼器自己保管
pub trait Decoder {
    /// 餵進一個 data payload。文字要即時印出來（串流手感就靠這個）
    fn push(&mut self, data: &str) -> Result<()>;
    /// 收尾，回傳這一輪助理訊息的內容塊（順序即原始順序）
    fn finish(self: Box<Self>) -> Vec<Block>;
}

/// 即時輸出串流文字
pub(crate) fn emit(text: &str) {
    print!("{text}");
    let _ = io::stdout().flush();
}

// ---------- 共用的串流迴圈：各家一模一樣 ----------

pub async fn stream_once(
    provider: &dyn Provider,
    client: &reqwest::Client,
    history: &[Message],
    tools: &[ToolDecl],
    system: &str,
) -> Result<Vec<Block>> {
    let resp = provider
        .request(client, history, tools, system)?
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("API error {}: {}", resp.status(), resp.text().await?);
    }

    let mut parser = SseParser::new();
    let mut decoder = provider.decoder();
    let mut bytes = resp.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        for data in parser.push(&chunk?) {
            decoder.push(&data)?;
        }
    }
    println!();
    Ok(decoder.finish())
}

// ---------- 依環境變數挑一家 ----------

pub fn from_env() -> Result<Box<dyn Provider>> {
    let which = std::env::var("RS_AGENT_PROVIDER").unwrap_or_else(|_| "gemini".to_string());
    match which.as_str() {
        "gemini" => Ok(Box::new(gemini::Gemini::from_env()?)),
        "openai" => Ok(Box::new(openai::OpenAiCompat::openai()?)),
        // Grok 與 Ollama 都是 OpenAI-compatible，同一份實作只換 base URL 與模型
        "grok" => Ok(Box::new(openai::OpenAiCompat::grok()?)),
        "ollama" => Ok(Box::new(openai::OpenAiCompat::ollama()?)),
        "claude" => Ok(Box::new(claude::Claude::from_env()?)),
        other => bail!(
            "不認識的 RS_AGENT_PROVIDER：{other}（可用：gemini / openai / grok / ollama / claude）"
        ),
    }
}
