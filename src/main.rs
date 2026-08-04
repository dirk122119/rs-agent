use anyhow::{Result, bail};
use futures::StreamExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
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

/// 把 "${VAR}" 換成環境變數的值——讓 .mcp.json 裡的 token 不用寫死，可以 commit
fn expand_env(s: &str) -> String {
    match s.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        Some(var) => std::env::var(var).unwrap_or_default(),
        None => s.to_string(),
    }
}

/// 依 .mcp.json 裡一個 server 的設定連線並完成握手：
/// 有 url 走 Streamable HTTP（遠端），有 command 則 spawn 子行程（本機 stdio）
async fn connect_mcp(cfg: &Value) -> Result<RunningService<RoleClient, ()>> {
    if let Some(url) = cfg["url"].as_str() {
        let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
        if let Some(token) = cfg["authToken"].as_str() {
            config = config.auth_header(expand_env(token));
        }
        let transport = StreamableHttpClientTransport::from_config(config);
        return Ok(().serve(transport).await?);
    }
    let Some(cmd) = cfg["command"].as_str() else {
        bail!("server 設定缺少 url 或 command: {cfg}");
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
/// 同時回傳 routes：工具名 → server 索引，呼叫時才知道要轉發給誰。
/// 本地的 read_skill 工具也在這裡追加，但不進 routes（它不屬於任何 server）
async fn tool_declarations(
    services: &[RunningService<RoleClient, ()>],
    skills: &[Skill],
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
    // 有技能才給 read_skill——沒技能還宣告這個工具，只會換來一次空轉
    if !skills.is_empty() {
        decls.push(json!({
            "name": "read_skill",
            "description": "讀取一個技能的完整說明。執行技能相關任務前先呼叫這個。",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "技能名稱"}
                },
                "required": ["name"]
            }
        }));
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

// ---------- Skills：漸進式揭露 ----------

struct Skill {
    name: String,
    description: String,
    path: std::path::PathBuf,
}

/// 掃 skills/<name>/SKILL.md，只解析 frontmatter 的 name/description，不讀內文
fn load_skills(dir: &str) -> Vec<Skill> {
    let mut skills = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return skills; // 沒有 skills 目錄就是零技能，不是錯誤
    };
    for entry in entries.flatten() {
        let path = entry.path().join("SKILL.md");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        // frontmatter = 開頭兩個 --- 之間的區塊
        let Some(rest) = raw.strip_prefix("---") else { continue };
        let Some((fm, _body)) = rest.split_once("---") else { continue };
        let field = |key: &str| {
            fm.lines()
                .find_map(|l| l.strip_prefix(&format!("{key}:")))
                .map(|v| v.trim().to_string())
        };
        let (Some(name), Some(description)) = (field("name"), field("description")) else {
            continue;
        };
        skills.push(Skill { name, description, path });
    }
    // read_dir 的順序未定義，排序讓 system prompt 可重現（也才吃得到 prefix cache）
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// system prompt 只放目錄：一行一個技能，全文等模型自己用 read_skill 拉
fn system_prompt(skills: &[Skill]) -> String {
    let mut s = String::from("你是 rs-agent，一個在終端機執行的助理。");
    if !skills.is_empty() {
        s.push_str(
            "\n\n# Skills\n\
             以下是可用的技能。當任務符合某個技能的描述時，\
             先用 read_skill 工具讀取全文，再照指示執行：\n",
        );
        for sk in skills {
            s.push_str(&format!("- {}: {}\n", sk.name, sk.description));
        }
    }
    s
}

// ---------- 呼叫模型一次（串流），回傳 (文字, 工具呼叫們) ----------

async fn stream_once(
    client: &reqwest::Client,
    api_key: &str,
    history: &[Value],
    tools: &Value,
    system: &str,
) -> Result<(String, Vec<Value>)> {
    let body = json!({
        "systemInstruction": {"parts": [{"text": system}]},
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

    // 技能目錄進 system prompt，全文等模型自己用 read_skill 拉
    let skills = load_skills("skills");
    let system = system_prompt(&skills);
    let (tools, routes) = tool_declarations(&services, &skills).await?;

    println!(
        "rs-agent  model={MODEL}  tools={}(MCP: {})  skills={}  (/quit 離開)",
        routes.len(),
        names.join(", "),
        skills.len()
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
            let (text, calls) = stream_once(&client, &api_key, &history, &tools, &system).await?;

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
                // read_skill 是本地工具，唯讀且模型碰不到路徑，不需要 confirm()
                let output = if name == "read_skill" {
                    let skill_name = c["args"]["name"].as_str().unwrap_or_default();
                    skills.iter().find(|s| s.name == skill_name).map_or_else(
                        || format!("unknown skill: {skill_name}"),
                        |s| std::fs::read_to_string(&s.path).unwrap_or_else(|e| format!("error: {e}")),
                    )
                } else if let Some(&i) = routes.get(name) {
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
