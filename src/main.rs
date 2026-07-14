use anyhow::{Result, bail};
use futures::StreamExt;
use serde_json::{Value, json};
use std::io::{self, Write};

const MODEL: &str = "gemini-2.5-flash";

// ---------- SSE parser（同 Step 3） ----------

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

// ---------- 工具定義與執行 ----------

/// 告訴模型有哪些工具可用（JSON Schema 描述參數）
fn tool_declarations() -> Value {
    json!([{
        "functionDeclarations": [
            {
                "name": "read_file",
                "description": "讀取一個文字檔並回傳內容",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "檔案路徑"}
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "run_command",
                "description": "執行 shell 指令並回傳 stdout+stderr",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "要執行的指令"}
                    },
                    "required": ["command"]
                }
            }
        ]
    }])
}

fn run_tool(name: &str, args: &Value) -> String {
    match name {
        "read_file" => {
            let path = args["path"].as_str().unwrap_or_default();
            std::fs::read_to_string(path).unwrap_or_else(|e| format!("error: {e}"))
        }
        "run_command" => {
            let cmd = args["command"].as_str().unwrap_or_default();
            match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
                Ok(o) => format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                ),
                Err(e) => format!("error: {e}"),
            }
        }
        _ => format!("unknown tool: {name}"),
    }
}

fn confirm() -> Result<bool> {
    print!("  執行嗎？ [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "yes"))
}

// ---------- 呼叫模型一次（串流），回傳 (文字, 工具呼叫們) ----------

async fn stream_once(
    client: &reqwest::Client,
    api_key: &str,
    history: &[Value],
) -> Result<(String, Vec<Value>)> {
    let body = json!({
        "contents": history,
        "tools": tool_declarations(),
    });
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
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut bytes = resp.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        for data in parser.push(&chunk?) {
            let Ok(v) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            let Some(parts) = v["candidates"][0]["content"]["parts"].as_array() else {
                continue;
            };
            for part in parts {
                if let Some(t) = part["text"].as_str() {
                    print!("{t}");
                    io::stdout().flush()?;
                    text.push_str(t);
                }
                if part["functionCall"].is_object() {
                    calls.push(part["functionCall"].clone());
                }
            }
        }
    }
    println!();
    Ok((text, calls))
}

// ---------- 主程式：REPL 外圈 + agent loop 內圈 ----------

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GEMINI_API_KEY")?;
    let client = reqwest::Client::new();
    let mut history: Vec<Value> = Vec::new();

    println!("rs-agent  model={MODEL}  tools=2  (/quit 離開)");
    loop {
        print!("❯ ");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/quit" {
            break;
        }

        history.push(json!({"role": "user", "parts": [{"text": line}]}));

        // agent loop：模型可能連續呼叫工具，直到不再呼叫為止
        loop {
            let (text, calls) = stream_once(&client, &api_key, &history).await?;

            // 把模型這輪的輸出（文字 + 工具呼叫）記回 history
            let mut parts = Vec::new();
            if !text.is_empty() {
                parts.push(json!({"text": text}));
            }
            for c in &calls {
                parts.push(json!({"functionCall": c}));
            }
            if !parts.is_empty() {
                history.push(json!({"role": "model", "parts": parts}));
            }

            if calls.is_empty() {
                break; // 模型不要工具了，這輪結束
            }

            // 執行每個工具呼叫，把結果組成 functionResponse
            let mut responses = Vec::new();
            for c in &calls {
                let name = c["name"].as_str().unwrap_or_default();
                println!("⏺ {name} {}", c["args"]);
                let output = if confirm()? {
                    run_tool(name, &c["args"])
                } else {
                    "使用者拒絕了這次工具呼叫".to_string()
                };
                println!("  ✓ {} bytes", output.len());
                responses.push(json!({
                    "functionResponse": {"name": name, "response": {"content": output}}
                }));
            }
            history.push(json!({"role": "user", "parts": responses}));
            // 繼續 loop：把工具結果送回去，讓模型接著做
        }
        println!();
    }
    Ok(())
}
