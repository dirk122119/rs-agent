# 從零打造 Rust Agent CLI 教學

一步一步做出一個「LLM + 工具 + 迴圈」的終端機 agent。每一步都是一個**能跑的完整版本**，做完先玩一下、commit，理解了再進下一步。

- 專案目錄建議：`~/CODE/rs-agent`（獨立新專案，不碰 rs-cli）
- Provider 用 **Gemini**（你已經有 `GEMINI_API_KEY`），最後一步會講怎麼換成別家
- 每步結束：`git add -A && git commit -m "step N"`，之後可以用 `git diff` 對照每步差異

## 事前準備

```bash
# 確認工具鏈
cargo --version          # 1.85+

# 確認 API key
echo $GEMINI_API_KEY     # 沒有的話去 https://aistudio.google.com/apikey 申請

# 建專案
cd ~/CODE
cargo new rs-agent
cd rs-agent
git init 2>/dev/null; git add -A && git commit -m "step 0: cargo new"
```

---

## Step 0 — CLI 骨架（~10 行）

**學什麼**：cargo 專案結構、讀命令列參數。

`src/main.rs`：

```rust
fn main() {
    let prompt: String = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if prompt.is_empty() {
        eprintln!("usage: rs-agent <prompt>");
        std::process::exit(1);
    }
    println!("你說：{prompt}");
}
```

**驗證**：

```bash
cargo run -- 你好
# 你說：你好
```

就這樣。重點只是確認 `cargo run` 的流程順了。

---

## Step 1 — 一次性 LLM 呼叫（~50 行）

**學什麼**：用 `reqwest` 發 HTTP、用 `serde_json::json!` 組 request body、從環境變數讀 key、解析回應 JSON。**先用 blocking（同步），不碰 async**——一次只學一件事。

加依賴：

```bash
cargo add reqwest --features blocking,json
cargo add serde_json anyhow
```

`src/main.rs` 全部換成：

```rust
use anyhow::{bail, Result};
use serde_json::{json, Value};

const MODEL: &str = "gemini-2.5-flash";

fn main() -> Result<()> {
    let prompt: String = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let api_key = std::env::var("GEMINI_API_KEY")?;

    let body = json!({
        "contents": [
            {"role": "user", "parts": [{"text": prompt}]}
        ]
    });

    let resp = reqwest::blocking::Client::new()
        .post(format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:generateContent"
        ))
        .header("x-goog-api-key", &api_key)
        .json(&body)
        .send()?;

    if !resp.status().is_success() {
        bail!("API error {}: {}", resp.status(), resp.text()?);
    }

    let v: Value = resp.json()?;
    let text = v["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("(no text)");
    println!("{text}");
    Ok(())
}
```

**看懂三件事**：

1. Gemini 的訊息格式是 `contents: [{role, parts: [{text}]}]`，role 只有 `user` 和 `model`
2. `Value` 是 serde_json 的萬用 JSON 型別，`v["a"]["b"]` 取不到時回 `Null` 而不是 panic，所以最後用 `.as_str()` 安全轉型
3. `?` 是 Rust 的錯誤傳播：任何一步失敗就直接從 `main` 回傳錯誤

**驗證**：

```bash
cargo run -- "用一句話解釋什麼是 ownership"
```

**常見錯誤**：`400 INVALID_ARGUMENT` 通常是 body 拼錯；`403` 是 key 無效；`environment variable not found` 是忘了 export key。

---

## Step 2 — REPL 與對話記憶（~70 行）

**學什麼**：對話「記憶」其實只是一個陣列——每輪把 user 和 model 的訊息都 push 進去，下次**整包**送出，模型就記得前文。

`src/main.rs`：

```rust
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::io::{self, Write};

const MODEL: &str = "gemini-2.5-flash";

fn call_gemini(client: &reqwest::blocking::Client, api_key: &str, history: &[Value]) -> Result<String> {
    let body = json!({"contents": history});
    let resp = client
        .post(format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:generateContent"
        ))
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()?;
    if !resp.status().is_success() {
        bail!("API error {}: {}", resp.status(), resp.text()?);
    }
    let v: Value = resp.json()?;
    Ok(v["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("(no text)")
        .to_string())
}

fn main() -> Result<()> {
    let api_key = std::env::var("GEMINI_API_KEY")?;
    let client = reqwest::blocking::Client::new();
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
        let reply = call_gemini(&client, &api_key, &history)?;
        println!("{reply}\n");
        history.push(json!({"role": "model", "parts": [{"text": reply}]}));
    }
    Ok(())
}
```

**驗證**（測「記憶」）：

```
❯ 我叫小陳
❯ 我剛剛說我叫什麼？    ← 模型應該答得出來
```

試著把最後一行 `history.push(...model...)` 註解掉再問一次——模型就「失憶」了。這一刻你會真正理解 LLM 是無狀態的，記憶完全是 client 端維護的。

---

## Step 3 — 串流輸出（~110 行）

**學什麼**：async/await 與 tokio、SSE（Server-Sent Events）格式、自己寫一個 20 行的 SSE parser。體感從「等 3 秒整段蹦出來」變成「即時逐字輸出」。

換依賴（blocking 換成 async + stream）：

```bash
cargo add tokio --features full
cargo add futures
cargo add reqwest --features json,stream
```

改動三處：

**(1) SSE parser** — SSE 是純文字協定，每個事件長這樣：`data: {...json...}\n\n`。因為網路 chunk 可能把事件切在任意位置，要自己緩衝。注意第一行的 `.replace("\r\n", "\n")`：Gemini 這個端點實際回傳的事件是用 `\r\n\r\n` 結尾（CRLF），不先正規化的話 `find("\n\n")` 永遠找不到事件邊界，程式會安靜地什麼都不輸出——這是實測踩到的坑，不是理論：

```rust
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
```

**(2) `call_gemini` 改成 async 串流版**，URL 換成 `:streamGenerateContent?alt=sse`：

```rust
use futures::StreamExt;

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
        for data in parser.push(&chunk?) {
            let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
            if let Some(t) = v["candidates"][0]["content"]["parts"][0]["text"].as_str() {
                print!("{t}");
                io::stdout().flush()?;   // 沒有這行就看不到逐字效果
                full_text.push_str(t);
            }
        }
    }
    println!("\n");
    Ok(full_text)
}
```

**(3) `main` 掛上 tokio**：

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // ... 同 Step 2，只是
    let client = reqwest::Client::new();                          // 拿掉 blocking
    let reply = call_gemini(&client, &api_key, &history).await?; // 加 .await
    // println!("{reply}") 拿掉——串流時已經印過了
```

**驗證**：問一個需要長回答的問題（「詳細解釋 Rust 的 borrow checker」），應該看到文字逐段流出來，而不是卡住後一次蹦出。

---

## Step 4 — Tool Calling：真正的 Agent Loop（~200 行）⭐ 核心

**學什麼**：這一步做完你就有一個真的 agent。本質就是：

```
把「工具清單」告訴模型
loop {
    呼叫模型
    模型說要用工具嗎？
      ├─ 否 → 這輪對話結束
      └─ 是 → 執行工具 → 把結果塞回 history → 繼續 loop
}
```

我們給模型兩個工具：`read_file`（讀檔）和 `run_command`（跑 shell 指令，執行前要按 y 確認——**永遠不要讓 LLM 無確認地跑指令**）。

`src/main.rs` 完整版：

```rust
use anyhow::{bail, Result};
use futures::StreamExt;
use serde_json::{json, Value};
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
            let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
            let Some(parts) = v["candidates"][0]["content"]["parts"].as_array() else { continue };
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
```

**驗證**：

```
❯ 讀取 Cargo.toml 然後告訴我這專案有哪些依賴
⏺ read_file {"path":"Cargo.toml"}
  執行嗎？ [y/N] y
  ✓ 312 bytes
（模型接著用檔案內容回答）

❯ 現在幾點？用指令查
⏺ run_command {"command":"date"}
  執行嗎？ [y/N] y
```

試試一個需要**連續多次**工具的任務（「列出 src 底下的檔案，然後讀其中最大的那個」），觀察 agent loop 跑了幾圈。

**這一步的核心觀念**：

1. 工具是你宣告給模型的「函式簽名」（JSON Schema），模型只會回「我想呼叫 X，參數是 Y」——**實際執行永遠是你的程式**
2. 工具結果以 `functionResponse` 塞回 history、role 是 `user`，對模型來說「工具結果」就是一種特殊的使用者訊息
3. 迴圈終止條件 = 模型的回應裡沒有 `functionCall`

---

## Step 5 — 接上 MCP：工具改由外部提供（~270 行）

**學什麼**：MCP（Model Context Protocol）是工具的「USB 標準」——工具由外部 server 提供，任何支援 MCP 的 agent 都能接上就用。這一步用 `rmcp` crate 當 MCP client：spawn server 子行程（stdio transport）、跟它要工具清單、把工具呼叫轉發給它。要接哪些 server 寫在 `.mcp.json` 設定檔裡（跟 Claude Code 的專案設定同一種格式），可以接多個。做完之後，Step 4 寫死的 `read_file`/`run_command` 就從你的程式碼裡消失了——agent 本體只剩「宣告轉換＋轉呼叫」，工具要幾個有幾個，加 server 也只是改設定檔。

前置需求：需要 Node.js（`npx`）。這是純 Rust 教學的唯一例外——現成穩定的 MCP server 生態以 npm 為主，我們用官方參考實作 `@modelcontextprotocol/server-filesystem` 來當對接目標（它剛好提供 `read_text_file`/`list_directory` 等工具，跟 Step 4 手寫的那兩個呼應：你自己寫的工具，換成標準化外部提供的）。

加依賴：

```bash
cargo add rmcp --no-default-features --features client,transport-child-process
```

改動六處：

**(1) `.mcp.json` 設定檔**（新檔案，放專案根目錄）——這是 Claude Code、Cursor 等工具通用的格式，`mcpServers` 底下每個 key 是一個 server，`command`/`args`/`env` 描述怎麼把它 spawn 起來：

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    }
  }
}
```

**(2) imports** — 檔案開頭加：

```rust
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use std::collections::HashMap;
use tokio::process::Command;
```

**(3) 連線 MCP server**（新函式）——吃一個 server 的設定 JSON，spawn 子行程、完成 MCP 的 initialize 握手，回傳一個能跟 server 對話的 client：

```rust
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
```

`()` 當 client handler 是 rmcp 的慣用寫法：我們只當「發請求的一方」，不需要處理 server 主動發來的請求，所以 handler 是空的。

**(4) `tool_declarations` 改成從 MCP 動態組**——刪掉整段寫死的 JSON，改成跟**每個** server 要清單、合併轉成 Gemini 格式。多了一個回傳值 `routes`（工具名 → server 索引）：工具清單合併之後，模型只會給你工具名，你得記得每個名字是哪個 server 的，呼叫時才知道轉發給誰：

```rust
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
```

這裡一定會踩到常見坑速查表裡那條 `400 Unknown name "$defs"`：真實的 MCP server（尤其 TypeScript/zod 產生的）吐出來的 JSON Schema 常帶 `$defs`/`$ref`/`additionalProperties`/`oneOf`，但 Gemini 的 tool schema 只收受限的 OpenAPI 子集。所以要多一個 `sanitize_schema`，遞迴走訪整個 schema、把不支援的 key 刪掉：

```rust
const UNSUPPORTED_KEYS: &[&str] = &[
    "$schema", "$id", "$comment", "$defs", "definitions", "$ref",
    "additionalProperties", "unevaluatedProperties", "patternProperties",
    "oneOf", "allOf", "not", "if", "then", "else", "const", "prefixItems",
];

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
```

（簡化版：直接把 `$ref` 刪掉，欄位會變成「無約束」。rs-cli 的完整版會先把 `$defs` 裡的定義 inline 回 `$ref` 的位置再刪，教學版先不做。）

**(5) `run_tool` 改成 async、轉發給 MCP**——不再自己讀檔跑指令，而是把呼叫包成 MCP 請求送給 server，收回文字結果：

```rust
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
```

`confirm()` 保留不動——工具的**來源**變了，但「執行前要人確認」的原則不變。

**(6) `main`**——進 REPL 前讀 `.mcp.json`、逐一連上每個 server、拿合併後的工具清單；`stream_once` 的簽名多帶一個 `tools: &Value`（body 裡的 `"tools"` 用它，不再呼叫舊的 `tool_declarations()`）；結束前逐一關掉連線：

```rust
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
    // ... REPL 迴圈同 Step 4，stream_once 多傳 &tools ...
    for mcp in services {
        mcp.cancel().await?;
    }
    Ok(())
```

工具呼叫處改成先查 `routes` 找到該轉發的 server；模型偶爾會幻覺出不存在的工具名，查不到就回個 error 讓它自己修正，不要 panic：

```rust
    let output = if let Some(&i) = routes.get(name) {
        if confirm()? {
            run_tool(&services[i], name, &c["args"]).await
        } else {
            "使用者拒絕了這次工具呼叫".to_string()
        }
    } else {
        format!("error: 未知的工具 {name}")
    };
```

**驗證**：

```
❯ 列出目前目錄的檔案，然後讀 Cargo.toml 告訴我有哪些依賴
⏺ list_directory {"path":"."}
  執行嗎？ [y/N] y
⏺ read_text_file {"path":"Cargo.toml"}
  執行嗎？ [y/N] y
（模型用檔案內容回答——但這次 read 的實作不在你的程式裡）
```

啟動時 banner 應該顯示 `tools=14(MCP: filesystem)` 左右——filesystem server 提供的工具比你 Step 4 手寫的兩個多得多，而你一行工具實作都沒寫。想多接一個 server，在 `.mcp.json` 加一段就好，程式碼一行都不用動。

**這一步的核心觀念**：

1. agent 程式碼從此**不含任何工具實作**——只做兩件事：把 server 的工具清單轉成 provider 的宣告格式、把模型的呼叫轉發回 server
2. MCP 的生命週期就三步：initialize（`connect_mcp` 裡的握手）→ `tools/list`（拿清單）→ `tools/call`（執行），你在 Step 4 學的 agent loop 完全不用改
3. **schema 相容性是接真實 server 時最大的坑**——provider 各自支援的 schema 子集不同，中間永遠需要一層 `sanitize_schema` 這樣的轉換
4. 多 server 之後，「工具名 → server」的路由表（`routes`）是必要的簿記——工具清單在 API 請求裡是攤平的，模型呼叫時只報名字不報出處

---

## Step 6 — Skills：漸進式揭露（~300 行）

**學什麼**：Skills 是給 agent 的「使用手冊」——把領域知識寫成 Markdown 檔，agent 需要時自己翻。重點是**漸進式揭露（progressive disclosure）**：system prompt 只放目錄（每個技能一行 name + description），全文等模型自己判斷需要時才用 `read_skill` 工具讀進 context。技能再多，平時只占目錄那幾行的 token；而且加技能＝加一個檔案，不用改程式、不用重編譯。

不加新依賴。frontmatter（SKILL.md 開頭 `---` 夾住的 metadata）用手刻解析——只需要 `name`/`description` 兩個欄位，為此引入 `serde_yaml` 不值得（rs-cli 用了 serde_yaml，因為它本來就有這個依賴；我們字串處理 20 行搞定）。

技能檔的格式長這樣——在專案裡建 `skills/commit-style/SKILL.md`：

```markdown
---
name: commit-style
description: 本專案 git commit 訊息的撰寫規範，寫 commit 訊息前必讀
---

# Commit 訊息規範

- 標題格式：`step-N: 一句話描述`（沿用本 repo 的分支慣例）
- 正文用繁體中文，解釋「為什麼改」而不是「改了什麼」
- 如果這次修改是 debug 的結果，把 debug 過程的關鍵發現寫進正文
```

改動四處：

**(1) `Skill` 結構 + `load_skills`**（新）——掃 `skills/<name>/SKILL.md`，只解析 frontmatter，不讀內文：

```rust
struct Skill {
    name: String,
    description: String,
    path: std::path::PathBuf,
}

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
    skills
}
```

**(2) system prompt**（新函式）——目錄只放一行一個技能，並告訴模型「先讀再做」：

```rust
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
```

送出的方式是 Gemini 的 `systemInstruction` 欄位——`stream_once` 的 body 改成：

```rust
    let body = json!({
        "systemInstruction": {"parts": [{"text": system}]},
        "contents": history,
        "tools": tools,
    });
```

（`stream_once` 簽名多帶一個 `system: &str`。順便你就學會了 system prompt 怎麼加——想給 agent 個性也是改這裡。）

**(3) `read_skill` 工具**——手動附加到 MCP 來的宣告清單後面（在 `tool_declarations` 裡 `decls.push(...)`）：

```rust
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
```

然後在 main 的工具分派處攔截：名字是 `read_skill` 就本地讀檔，其他照舊走 MCP。`read_skill` 不需要 `confirm()`——它唯讀、路徑受控（只能讀 `load_skills` 掃到的那幾個檔案），跟「跑任意 shell 指令」的風險等級完全不同：

```rust
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
```

**(4) `main`**——啟動時 `let skills = load_skills("skills");`、`let system = system_prompt(&skills);`，banner 加上 `skills={}`（用 `skills.len()`）。

**驗證**（重點是驗「漸進式揭露」兩面都成立）：

```
❯ 1+1 等於多少
2
（模型不會呼叫 read_skill——目錄裡的 description 判斷跟這個問題無關，
 技能全文一個 token 都沒花）

❯ 幫我為這次修改寫一個 commit 訊息
⏺ read_skill {"name":"commit-style"}
  ✓ 214 bytes
（模型讀完規範，照著「step-N: 開頭、繁體中文、寫為什麼」的格式產出）
```

**這一步的核心觀念**：

1. system prompt 只放**索引**，內容**按需載入**——這就是 context 工程最基本的一招：不是塞更多，而是讓模型自己決定何時拉取
2. 技能就是檔案——加技能不用改程式碼、不用重編譯，丟一個 `SKILL.md` 進去就生效
3. `description` 的品質直接決定模型會不會在對的時機想起這個技能——寫「本專案 git commit 訊息的撰寫規範，寫 commit 訊息前必讀」而不是「commit 相關」

---

## 結語 — 從 rs-agent 到 rs-cli

到這裡你已經有一個約 300 行、接得上 MCP 生態、帶 Skills 的完整 agent。rs-cli 做的事就是在這個骨架上繼續疊工程化的東西，對照著讀：

| rs-agent 的做法 | rs-cli 的做法 | 檔案 |
|---|---|---|
| 寫死 Gemini | `Provider` trait + 三個實作，config 切換 | `src/provider/mod.rs` |
| 訊息用 `serde_json::Value` 硬組 | 內部統一 `Message`/`ContentBlock` 型別，各 provider 自己轉換 | `src/provider/types.rs` |
| 多 server 但同名工具先到先贏 | 工具名加 `server__` 前綴避免衝突，單一 server 掛掉不影響其他 | `src/mcp/manager.rs` |
| main 裡一坨 loop | agent loop 抽成 `Agent::run_turn`，UI 用 callback 解耦 | `src/agent.rs` |
| history 無限長 | 超過字元預算時裁掉舊 turn（注意不能拆散 tool call/result 配對） | `agent.rs` 的 `trim_history` |
| `sanitize_schema` 直接刪 `$ref` | 先把 `$defs` 定義 inline 回去再清理（深度限制防循環） | `src/provider/gemini.rs` |

建議練習（依難度排序）：

1. **在 `.mcp.json` 多接一個 server**（例如 `@modelcontextprotocol/server-memory`），然後把「同名工具先到先贏」改成不會衝突的做法（提示：rs-cli 用 `server__tool` 前綴——宣告時加上去、呼叫時拆回來）
2. **加第二個技能**，感受「加技能不用改程式碼」
3. **抽 Provider trait**：定義 `trait Provider { async fn chat(...) }`，先做 Gemini 實作，再加一個 Ollama（OpenAI-compat）實作——做完你就懂為什麼 rs-cli 要有 `types.rs`

---

## 常見坑速查

| 症狀 | 原因 |
|---|---|
| `400 Unknown name "$defs" ... parameters` | Gemini 的 tool schema 是受限的 OpenAPI 子集，不接受 `$ref`/`$defs`/`additionalProperties`/`oneOf`（Step 5 的 `sanitize_schema` 就是在處理這個） |
| 串流完全沒輸出、也不報錯 | SSE 事件邊界沒抓到——Gemini 用 `\r\n\r\n` 結尾，`find("\n\n")` 前要先 `.replace("\r\n", "\n")`（Step 3 的 SseParser 第一行） |
| 串流看不到逐字效果 | `print!` 之後忘了 `io::stdout().flush()` |
| 模型「失憶」 | 忘了把 model 回覆 push 回 history |
| tool loop 跑不停 | 工具回傳錯誤訊息但格式讓模型誤解，或沒把 `functionResponse` 塞回 history |
| `429 RESOURCE_EXHAUSTED` | 免費額度限流，換 `gemini-2.5-flash-lite` 或稍等 |
