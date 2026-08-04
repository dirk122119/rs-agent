mod provider;

use anyhow::{Result, bail};
use provider::{Block, Message, ToolDecl};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Write};
use tokio::process::Command;

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

/// 跟每個 MCP server 要工具清單，合併成中性的 ToolDecl。
/// 同時回傳 routes：工具名 → server 索引，呼叫時才知道要轉發給誰。
/// 注意這裡不再對 schema 做任何清理——那是 Gemini 專屬的限制，已經搬進 provider/gemini.rs
async fn tool_declarations(
    services: &[RunningService<RoleClient, ()>],
    skills: &[Skill],
) -> Result<(Vec<ToolDecl>, HashMap<String, usize>)> {
    let mut decls = Vec::new();
    let mut routes = HashMap::new();
    for (i, mcp) in services.iter().enumerate() {
        for t in mcp.list_all_tools().await? {
            if routes.contains_key(t.name.as_ref()) {
                continue; // 同名工具以先連上的 server 為準
            }
            routes.insert(t.name.to_string(), i);
            decls.push(ToolDecl {
                name: t.name.to_string(),
                description: t.description.as_deref().unwrap_or("").to_string(),
                schema: Value::Object((*t.input_schema).clone()),
            });
        }
    }
    // 有技能才給 read_skill——沒技能還宣告這個工具，只會換來一次空轉
    if !skills.is_empty() {
        decls.push(ToolDecl::read_skill());
    }
    Ok((decls, routes))
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

// ---------- 主程式：REPL 外圈 + agent loop 內圈 ----------

#[tokio::main]
async fn main() -> Result<()> {
    // 哪家模型由 RS_AGENT_PROVIDER 決定，agent loop 本身完全不知道差別
    let prov = provider::from_env()?;
    let client = reqwest::Client::new();
    let mut history: Vec<Message> = Vec::new();

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
        "rs-agent  {}/{}  tools={}(MCP: {})  skills={}  (/quit 離開)",
        prov.name(),
        prov.model(),
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

        history.push(Message::user_text(line));

        // agent loop：模型可能連續呼叫工具，直到不再呼叫為止
        loop {
            let blocks = provider::stream_once(&*prov, &client, &history, &tools, &system).await?;

            // 先把工具呼叫抄成 owned，blocks 隨後要搬進 history
            let calls: Vec<(String, String, Value)> = provider::tool_calls(&blocks)
                .into_iter()
                .map(|(id, name, args)| (id.to_string(), name.to_string(), args.clone()))
                .collect();

            // 模型這輪的輸出原封不動記回 history——包含 provider 專屬的塊
            // （例如 Claude 的 thinking，必須原樣送回否則 400）
            if !blocks.is_empty() {
                history.push(Message::assistant(blocks));
            }

            if calls.is_empty() {
                break; // 模型不要工具了，這輪結束
            }

            // 執行每個工具呼叫，把結果組成中性的 ToolResult
            let mut results = Vec::new();
            for (id, name, args) in &calls {
                println!("⏺ {name} {args}");
                // read_skill 是本地工具，唯讀且模型碰不到路徑，不需要 confirm()
                let output = if name == "read_skill" {
                    let skill_name = args["name"].as_str().unwrap_or_default();
                    skills.iter().find(|s| s.name == skill_name).map_or_else(
                        || format!("unknown skill: {skill_name}"),
                        |s| std::fs::read_to_string(&s.path).unwrap_or_else(|e| format!("error: {e}")),
                    )
                } else if let Some(&i) = routes.get(name.as_str()) {
                    if confirm()? {
                        run_tool(&services[i], name, args).await
                    } else {
                        "使用者拒絕了這次工具呼叫".to_string()
                    }
                } else {
                    format!("error: 未知的工具 {name}")
                };
                println!("  ✓ {} bytes", output.len());
                results.push(Block::ToolResult {
                    id: id.clone(),
                    name: name.clone(),
                    content: output,
                });
            }
            history.push(Message::tool_results(results));
            // 繼續 loop：把工具結果送回去，讓模型接著做
        }
        println!();
    }

    for mcp in services {
        mcp.cancel().await?;
    }
    Ok(())
}
