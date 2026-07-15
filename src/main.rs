use anyhow::{Result, bail};
use futures::StreamExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, Write};
use tokio::process::Command;

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

// ---------- MCP：設定、連線、工具宣告、工具執行 ----------

/// 依 .mcp.json 裡一個 server 的設定（command/args/env）spawn 子行程並完成握手
async fn connect_mcp(cfg: &Value) -> Result<RunningService<RoleClient, ()>> {
    let Some(cmd) = cfg["command"].as_str() else {
        bail!("server 設定缺少 command: {cfg}");
    };
    let mut c = Command::new(cmd);
    if let Some(args) = cfg["args"].as_array() {
        c.args(args.iter().filter_map(|a| a.as_str()));
    }
    if let Some(env) = cfg["env"].as_object() {
        for (k, v) in env {
            c.env(k, v.as_str().unwrap_or_default());
        }
    }
    let transport = TokioChildProcess::new(c)?;
    Ok(().serve(transport).await?)
}

/// 跟每個 MCP server 要工具清單，合併轉成 Gemini 的 functionDeclarations 格式。
/// 同時回傳 routes：工具名 → server 索引，呼叫時才知道要轉發給誰
async fn tool_declarations(
    services: &[RunningService<RoleClient, ()>],
) -> Result<(Value, HashMap<String, usize>)> {
    let mut decls: Vec<Value> = Vec::new();
    let mut routes = HashMap::new();
    for (i, mcp) in services.iter().enumerate() {
        for t in mcp.list_all_tools().await? {
            if routes.contains_key(t.name.as_ref()) {
                continue; // 同名工具以先連上的 server 為準
            }
            routes.insert(t.name.to_string(), i);
            decls.push(json!({
                "name": t.name,
                "description": t.description.as_deref().unwrap_or(""),
                "parameters": sanitize_schema(&Value::Object((*t.input_schema).clone())),
            }));
        }
    }
    Ok((json!([{"functionDeclarations": decls}]), routes))
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

async fn run_tool(mcp: &RunningService<RoleClient, ()>, name: &str, args: &Value) -> String {
    let mut params = CallToolRequestParams::new(name.to_string());
    params.arguments = args.as_object().cloned();
    match mcp.call_tool(params).await {
        Ok(r) => r
            .content
            .iter()
            .filter_map(|b| b.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(e) => format!("error: {e}"),
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
    tools: &Value,
) -> Result<(String, Vec<Value>)> {
    let body = json!({
        "contents": history,
        "tools": tools,
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

    // 讀 .mcp.json（與 Claude Code 等工具相同格式），逐一連上每個 MCP server
    let config: Value = serde_json::from_str(&std::fs::read_to_string(".mcp.json")?)?;
    let Some(server_cfgs) = config["mcpServers"].as_object() else {
        bail!(".mcp.json 裡找不到 mcpServers");
    };
    let mut names = Vec::new();
    let mut services = Vec::new();
    for (name, cfg) in server_cfgs {
        services.push(connect_mcp(cfg).await?);
        names.push(name.as_str());
    }
    let (tools, routes) = tool_declarations(&services).await?;

    println!(
        "rs-agent  model={MODEL}  tools={}(MCP: {})  (/quit 離開)",
        routes.len(),
        names.join(", ")
    );
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
            let (text, calls) = stream_once(&client, &api_key, &history, &tools).await?;

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
                let output = if let Some(&i) = routes.get(name) {
                    if confirm()? {
                        run_tool(&services[i], name, &c["args"]).await
                    } else {
                        "使用者拒絕了這次工具呼叫".to_string()
                    }
                } else {
                    format!("error: 未知的工具 {name}")
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

    for mcp in services {
        mcp.cancel().await?;
    }
    Ok(())
}
