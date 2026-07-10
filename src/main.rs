use anyhow::{Result, bail};
use futures::StreamExt;
use serde_json::{Value, json};
use std::io::{self, Write};

const MODEL: &str = "gemini-2.5-flash";

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

async fn call_gemini(client: &reqwest::Client, api_key: &str, history: &[Value]) -> Result<String> {
    let body = json!({"contents": history});
    let resp = client
        .post(format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:streamGenerateContent?alt=sse"
        ))
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("API error {}: {}", resp.status(), resp.text().await?);
    }

    let mut parser = SseParser::new();
    let mut full_text = String::new();
    let mut bytes = resp.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk?;
        for data in parser.push(&chunk) {
            let Ok(v) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            if let Some(t) = v["candidates"][0]["content"]["parts"][0]["text"].as_str() {
                print!("{t}");
                io::stdout().flush()?;
                full_text.push_str(t);
            }
        }
    }
    println!("\n");
    Ok(full_text)
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GEMINI_API_KEY")?;
    let client = reqwest::Client::new();
    let mut history: Vec<Value> = Vec::new();

    println!("rs-agent  model={MODEL}  (/quit 離開)");
    loop {
        print!("❯ ");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            break; // Ctrl-D
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/quit" {
            break;
        }

        history.push(json!({"role": "user", "parts": [{"text": line}]}));
        let reply = call_gemini(&client, &api_key, &history).await?;
        history.push(json!({"role": "model", "parts": [{"text": reply}]}));
    }
    Ok(())
}
