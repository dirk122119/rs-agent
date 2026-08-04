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

**延伸：接線上的 server（Streamable HTTP）**

MCP 的 transport 除了 stdio（spawn 本機子行程）還有 Streamable HTTP——直接連遠端 URL。加一個 feature：

```bash
cargo add rmcp --no-default-features \
  --features client,transport-child-process,transport-streamable-http-client-reqwest
```

`connect_mcp` 開頭加一個分支，設定裡有 `url` 就走 HTTP（`.mcp.json` 的遠端寫法也跟 Claude Code 相同）：

```rust
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};

async fn connect_mcp(cfg: &Value) -> Result<RunningService<RoleClient, ()>> {
    if let Some(url) = cfg["url"].as_str() {
        let transport = StreamableHttpClientTransport::from_uri(url.to_string());
        return Ok(().serve(transport).await?);
    }
    // ...原本的 command/args/env 分支照舊...
```

```json
    "deepwiki": {
      "type": "http",
      "url": "https://mcp.deepwiki.com/mcp"
    }
```

重點在於：兩種 transport 的 `().serve(transport)` 回傳同一個 `RunningService` 型別，所以 `tool_declarations`、`routes`、`run_tool` **全都不用改**——transport 被抽象掉了，這正是 MCP「USB 標準」的意思。DeepWiki 是公開免認證的 server（提供讀 GitHub repo 文件的工具），拿來驗證剛好。

**遠端 server 的認證**

實務上多數線上 server 要帶 token。rmcp 的 config 有現成的 `auth_header()`，所以不用自己組 `reqwest::Client`——設定檔多一個 `authToken` 欄位就好：

```json
    "someServer": {
      "type": "http",
      "url": "https://example.com/mcp",
      "authToken": "${MY_SERVER_TOKEN}"
    }
```

值寫成 `${VAR}` 形式、真正的 token 放環境變數，`.mcp.json` 才能安心 commit 進 repo。展開的函式很短：

```rust
/// 把 "${VAR}" 換成環境變數的值——讓 .mcp.json 裡的 token 不用寫死，可以 commit
fn expand_env(s: &str) -> String {
    match s.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        Some(var) => std::env::var(var).unwrap_or_default(),
        None => s.to_string(),
    }
}
```

`connect_mcp` 的 HTTP 分支從 `from_uri` 改成先建 config，有 token 就掛上去：

```rust
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

    if let Some(url) = cfg["url"].as_str() {
        let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
        if let Some(token) = cfg["authToken"].as_str() {
            config = config.auth_header(expand_env(token));
        }
        let transport = StreamableHttpClientTransport::from_config(config);
        return Ok(().serve(transport).await?);
    }
```

`auth_header()` 收的是**不含 `Bearer ` 前綴**的 raw token，rmcp 自己組成 `Authorization: Bearer <token>` 送出。

順帶注意兩種 transport 傳認證資料的慣例不同：stdio 分支的 `cfg["env"]` 設的是**子行程**的環境變數（server 自己去讀），而 `expand_env` 讀的是 **rs-agent 這個行程**的環境變數，展開後塞進 HTTP header。前者把秘密交給 server 進程，後者由 client 自己帶著。

**這一步的核心觀念**：

1. agent 程式碼從此**不含任何工具實作**——只做兩件事：把 server 的工具清單轉成 provider 的宣告格式、把模型的呼叫轉發回 server
2. MCP 的生命週期就三步：initialize（`connect_mcp` 裡的握手）→ `tools/list`（拿清單）→ `tools/call`（執行），你在 Step 4 學的 agent loop 完全不用改
3. **schema 相容性是接真實 server 時最大的坑**——provider 各自支援的 schema 子集不同，中間永遠需要一層 `sanitize_schema` 這樣的轉換
4. 多 server 之後，「工具名 → server」的路由表（`routes`）是必要的簿記——工具清單在 API 請求裡是攤平的，模型呼叫時只報名字不報出處

---

## Step 6 — Skills：漸進式揭露（~370 行）

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
    // read_dir 的順序未定義，排序讓 system prompt 可重現（也才吃得到 prefix cache）
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}
```

那行排序不是潔癖：`std::fs::read_dir` 不保證順序，不排的話 system prompt 的技能目錄會因機器、因檔案增刪而變動——同樣的問題問兩次可能得到不同的 prompt 前綴，既不好 debug，也讓 Gemini 的隱式 prefix cache 失效。

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

（`stream_once` 簽名多帶一個 `system: &str`，**加在參數列最後**。別放在 `api_key` 旁邊——兩個都是 `&str` 又相鄰，呼叫端萬一寫反了型別檢查不會攔，結果是把 API key 當成 system prompt 送出去。放最後則相鄰參數型別各不相同，寫反直接編譯失敗。順便你就學會了 system prompt 怎麼加——想給 agent 個性也是改這裡。）

**(3) `read_skill` 工具**——手動附加到 MCP 來的宣告清單後面。`tool_declarations` 簽名多收一個 `skills: &[Skill]`，在 `return` 前 push；注意它**不進 `routes`**，因為它背後沒有任何 server：

```rust
async fn tool_declarations(
    services: &[RunningService<RoleClient, ()>],
    skills: &[Skill],
) -> Result<(Value, HashMap<String, usize>)> {
    // ...原本走訪每個 server 的迴圈照舊...

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
```

`!skills.is_empty()` 這個條件值得一提：沒有 `skills/` 目錄時還把 `read_skill` 宣告出去，等於給模型一個背後空無一物的工具——它最好的下場是浪費一次來回、拿到 `unknown skill` 再自己修正。工具宣告要跟實際能力對齊。

然後在 main 的工具分派處攔截：名字是 `read_skill` 就本地讀檔，其他照舊走 MCP。`read_skill` 不需要 `confirm()`——它唯讀，而且模型從頭到尾碰不到路徑的任何一段：模型給的字串只拿去跟 `s.name` 做等值比對，真正打開的是 `load_skills` 啟動時掃到的 `s.path`。沒有路徑穿越的施力點，跟「跑任意 shell 指令」的風險等級完全不同：

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

**(4) `main`**——啟動時 `let skills = load_skills("skills");`、`let system = system_prompt(&skills);`，banner 加上 `skills={}`（用 `skills.len()`）。這兩行要放在 `tool_declarations(&services, &skills)` **之前**，因為現在得把 skills 傳進去。

banner 的 `tools={}` 維持用 `routes.len()`，不要改成 `routes.len() + 1`——那是把「內建工具剛好只有一個」寫死進算式，日後多一個就默默算錯；而字串本身已經用 `(MCP: ...)` 界定了那個數字的範圍，`skills={}` 則涵蓋新增的那一塊。

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
2. 技能就是檔案——加技能不用改程式碼、不用重編譯，丟一個 `SKILL.md` 進去、重啟就生效（`load_skills` 只在啟動時掃一次；想做到丟進去立刻生效，把它移到 REPL 迴圈開頭每輪重掃即可）
3. `description` 的品質直接決定模型會不會在對的時機想起這個技能——寫「本專案 git commit 訊息的撰寫規範，寫 commit 訊息前必讀」而不是「commit 相關」

---

## Step 7 — Provider 抽象：Gemini / OpenAI / Grok / Ollama / Claude（~1050 行、拆成模組）

**學什麼**：把「哪家模型」變成一個可替換的零件。做完之後 `RS_AGENT_PROVIDER=claude` 就換一家，agent loop 一行都不用改。

這是目前為止最大的一步，而且**難的地方不在 HTTP**。各家的 endpoint 和認證 header 不同是小事（各三行），真正的工程量在**訊息格式全都不一樣**——你現在的 `history: Vec<Value>` 直接就是 Gemini 的格式，這條路走不下去。

先看清差異在哪：

| | Gemini | OpenAI / Grok | Claude |
|---|---|---|---|
| endpoint | `models/{m}:streamGenerateContent?alt=sse` | `/v1/chat/completions` | `/v1/messages` |
| 認證 | `x-goog-api-key` | `Authorization: Bearer` | `x-api-key` + `anthropic-version` |
| 訊息欄位 | `contents` | `messages` | `messages` |
| 助理角色名 | `model` | `assistant` | `assistant` |
| system prompt | `systemInstruction` 欄位 | `messages[0]` role=system | **頂層 `system` 欄位** |
| 工具宣告 | `tools[0].functionDeclarations[]`、`parameters` | `tools[].function{}`、`parameters` | `tools[]`、**`input_schema`** |
| 工具結果怎麼配對 | 用**工具名** | 用 **id**（`tool_call_id`） | 用 **id**（`tool_use_id`） |
| 工具結果放哪 | `user` 訊息的 part | **獨立的 role="tool" 訊息，一個結果一則** | `user` 訊息的 content 塊 |
| 串流裡的工具參數 | 完整 JSON 物件 | **字串碎片**，要累積 | **字串碎片**，要累積 |
| `max_tokens` | 選填 | 選填 | **必填** |
| schema 子集 | 受限，要 sanitize | 較完整 | 較完整 |

**Grok 和 Ollama 幾乎免費**：xAI 與 Ollama 都是 OpenAI-compatible，同一份請求格式，只換 base URL（`api.x.ai/v1`、`localhost:11434/v1`）和模型名。所以**五家只需要三個實作**。

**(1) 拆檔案**——單一 `main.rs` 到這裡會爆掉，拆成：

```
src/
  main.rs              REPL + agent loop + MCP + Skills
  provider/
    mod.rs             兩個 trait、共用串流迴圈、SseParser、依環境變數挑一家
    types.rs           中性訊息型別
    gemini.rs
    openai.rs          OpenAI 與 Grok 共用
    claude.rs
```

`main.rs` 開頭加 `mod provider;` 就好，Rust 會自己去找 `src/provider/mod.rs`。

**(2) 中性訊息型別**（`provider/types.rs`）——這是整步的核心。各家格式的最小公倍數：

```rust
#[derive(Clone, Copy, PartialEq)]
pub enum Role { User, Assistant }

#[derive(Clone)]
pub enum Block {
    Text(String),
    /// 模型要求呼叫工具。id 是給 OpenAI/Claude 配對用的，Gemini 改用 name 配對
    ToolCall { id: String, name: String, args: Value },
    /// 工具結果。id 與 name 都帶著，因為各家配對方式不同
    ToolResult { id: String, name: String, content: String },
    /// 某家 provider 專屬、且必須原封不動送回的塊（例如 Claude 的 thinking）。
    /// 產生它的 provider 認得它，其他 provider 直接跳過
    Opaque(Value),
}

pub struct Message { pub role: Role, pub blocks: Vec<Block> }
```

兩個設計決定值得說明。**`ToolResult` 同時帶 `id` 和 `name`**：不是冗餘，是因為 Gemini 拿名字配對、另兩家拿 id 配對，中性型別必須同時滿足。**`Block::Opaque`** 是給「看不懂但必須原樣送回」的塊用的——Claude 的 thinking block 就是這種，少了它多輪對話會 400。

工具宣告也要中性化，直接帶 MCP 給的原始 schema，清不清理交給各 provider：

```rust
pub struct ToolDecl { pub name: String, pub description: String, pub schema: Value }
```

`sanitize_schema` 因此從 `tool_declarations` 搬進 `provider/gemini.rs`——它是 Gemini 專屬的限制，不該汙染其他家。

**(3) 兩個 trait**（`provider/mod.rs`）——這裡有個關鍵觀察：

> **HTTP 與 SSE 的串流迴圈各家完全一樣，只有「怎麼組請求」與「怎麼解事件」不同。**

所以 trait 全是**同步**方法，非同步的部分由共用函式寫一次：

```rust
pub trait Provider {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    /// 組出一個「還沒送出」的請求：URL、認證 header、body 都在這裡決定
    fn request(&self, client: &reqwest::Client, history: &[Message],
               tools: &[ToolDecl], system: &str) -> Result<reqwest::RequestBuilder>;
    /// 每次請求開一個新的解碼器（它要保存跨事件的累積狀態）
    fn decoder(&self) -> Box<dyn Decoder>;
}

pub trait Decoder {
    /// 餵進一個 data payload。文字要即時印出來
    fn push(&mut self, data: &str) -> Result<()>;
    /// 收尾，回傳這一輪助理訊息的內容塊
    fn finish(self: Box<Self>) -> Vec<Block>;
}
```

同步的好處不只是簡單：**Rust 的 async fn in trait 不能做成 trait object**，而我們需要 `Box<dyn Provider>`（執行期才知道要哪家）。切成同步就天然 dyn-compatible，不必引入 `async_trait` 依賴。

共用的串流迴圈就是原本的 `stream_once` 拿掉 Gemini 專屬部分：

```rust
pub async fn stream_once(
    provider: &dyn Provider, client: &reqwest::Client,
    history: &[Message], tools: &[ToolDecl], system: &str,
) -> Result<Vec<Block>> {
    let resp = provider.request(client, history, tools, system)?.send().await?;
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
```

`SseParser`（Step 3 寫的）也搬進來共用——三家都是 `data:` 行，Claude 雖然多送 `event:` 行但 payload 自帶 `"type"` 欄位，不看 `event:` 也能解。

**(4) 三個實作的怪癖**

*Gemini*：每個 chunk 都是完整 JSON，工具呼叫是完整物件，不需要累積。它不給工具呼叫 id，所以解碼器補一個序號讓中性型別完整（反正 Gemini 用名字配對，id 用不到）。

*OpenAI / Grok*：工具參數在串流裡是碎片，按 `delta.tool_calls[].index` 分組累積，`id` 與 `name` 只在該 index 的第一個碎片出現：

```rust
if let Some(a) = tc["function"]["arguments"].as_str() {
    slot.json.push_str(a);       // 累積碎片
}
// ...全部收完才 parse
args: serde_json::from_str(&c.json).unwrap_or_else(|_| json!({})),
```

還有一個結構性差異：工具結果是**獨立的 `role="tool"` 訊息**，所以「一則中性訊息」會展開成**多則** wire 訊息。這是為什麼中性型別要能一對多轉換。另外 `arguments` 是 JSON **字串**不是物件，送回去時要 `args.to_string()`。串流結尾會多送一個 `data: [DONE]`，解碼器直接忽略。

*Claude*：SSE 是具型別的事件流，用 `index` 開／關內容塊：

```
content_block_start  {index, content_block: {type: "text"|"tool_use"|"thinking", ...}}
content_block_delta  {index, delta: {type: "text_delta"|"input_json_delta"|"thinking_delta", ...}}
content_block_stop   {index}
message_delta        {delta: {stop_reason}}
```

解碼器用 `BTreeMap<u64, Slot>` 存槽位，收尾時 index 順序即原始順序。最容易踩的坑是 **thinking**：`claude-opus-5` 預設開啟思考，而多輪對話**必須把 thinking block 原封不動送回**（含 `signature`），改動或刪掉都會 400。處理方式不是特別為它寫程式，而是讓解碼器**忠實重建整個 content 陣列**——認得的塊轉成 `Text`/`ToolCall`，不認得的原樣存進 `Block::Opaque`，送回時直接吐出來。text / thinking / tool_use 一體適用，比過濾簡單。

（順帶一提：關掉思考不是好選擇。官方文件明確警告，`thinking: {"type": "disabled"}` 在工具密集的 agent 上會出現「模型把工具呼叫寫成純文字、呼叫靜默不執行」的失敗模式——不報錯、不 panic，只是那一輪什麼都沒做。你有十幾個 MCP 工具，正是最容易中的情境。）

**(5) 切換與驗證**

```bash
RS_AGENT_PROVIDER=gemini  GEMINI_API_KEY=...     cargo run   # 預設
RS_AGENT_PROVIDER=openai  OPENAI_API_KEY=...     cargo run
RS_AGENT_PROVIDER=grok    XAI_API_KEY=...        cargo run
RS_AGENT_PROVIDER=claude  ANTHROPIC_API_KEY=...  cargo run
RS_AGENT_PROVIDER=ollama                         cargo run   # 本機，不需要金鑰
```

模型名用 `RS_AGENT_MODEL` 覆寫（各家有預設值）。banner 會顯示挑到哪一家：

```
rs-agent  claude/claude-opus-5  tools=17(MCP: deepwiki, filesystem)  skills=1  (/quit 離開)
```

**抽象有沒有付清成本？** 第五家（Ollama）是最好的檢驗：它只花了 **13 行**——`openai.rs` 加一個建構子（因為 Ollama 也是 OpenAI-compatible），`from_env` 加一個分支。

```rust
/// 本機 Ollama。不需要金鑰，但 OpenAI 的 wire format 仍要有 Authorization，
/// 所以塞一個佔位字串——這裡不能用 `var()?`，否則沒設環境變數就啟動失敗
pub fn ollama() -> Result<Self> {
    Ok(Self {
        name: "ollama",
        base: "http://localhost:11434/v1",
        api_key: std::env::var("OLLAMA_API_KEY").unwrap_or_else(|_| "ollama".to_string()),
        model: model_or("qwen3"),
    })
}
```

如果你加第五家要動到 `main.rs`、動到 agent loop、或動到中性型別，那就是接縫切錯了——回頭看 (2) 和 (3)。

真正要驗的是**同一段對話在各家之間行為一致**：問一個需要用工具的問題（「讀 Cargo.toml 告訴我有哪些依賴」），確認每一家都能走完 `工具呼叫 → confirm → 結果回填 → 模型接著答` 這一圈。工具結果配對錯了的話症狀很明顯——模型會說它沒收到結果，或直接重複呼叫同一個工具。

**Ollama 是唯一不需要金鑰的驗證路徑**，所以拿它把 OpenAI-compat 那條路（partial JSON 累積、`role="tool"` 拆訊息）實測到底最划算——同一份程式碼驗過了，OpenAI 與 Grok 就只剩 base URL 和模型名的差別。一個提醒：本機小模型的 tool calling 通常很弱，而你會一次餵給它十幾個 MCP 工具。測試時先把 `.mcp.json` 縮到只留 filesystem，否則分不清是接線錯了還是模型接不住。

**這一步的核心觀念**：

1. **抽象要抽在對的接縫上**。直覺會想「每家寫一個 `async fn chat()`」，但那會把四份幾乎一樣的 HTTP/SSE 迴圈複製四遍。找到真正的變異點（組請求、解事件）之後，共用的部分反而更多。
2. **中性型別的欄位是被最嚴格的那家決定的**。`ToolResult` 帶 id 又帶 name 看起來冗餘，但少任一個就有一家接不起來。這就是 rs-cli 要有 `types.rs` 的原因。
3. **「必須原樣送回」是真實存在的約束**。`Block::Opaque` 不是設計潔癖——provider 會有你不該解讀、但必須保留的狀態（Claude 的 thinking signature、將來的加密推理塊）。中性型別要留這個逃生口。
4. 同步 trait + 共用非同步驅動，是 Rust 裡繞開「async trait 不能 dyn」的標準手法，而且順便讓抽象更小。

---

## 結語 — 從 rs-agent 到 rs-cli

到這裡你已經有一個約 1050 行、接得上 MCP 生態、帶 Skills、五家模型可換的完整 agent（`main.rs` 264 行 + `provider/` 五個檔案）。rs-cli 做的事就是在這個骨架上繼續疊工程化的東西，對照著讀：

| rs-agent 的做法 | rs-cli 的做法 | 檔案 |
|---|---|---|
| 用 `RS_AGENT_PROVIDER` 環境變數挑 provider | config 檔管理 provider／模型／金鑰，可 per-project 覆寫 | `src/config.rs` |
| 多 server 但同名工具先到先贏 | 工具名加 `server__` 前綴避免衝突，單一 server 掛掉不影響其他 | `src/mcp/manager.rs` |
| main 裡一坨 loop | agent loop 抽成 `Agent::run_turn`，UI 用 callback 解耦 | `src/agent.rs` |
| history 無限長 | 超過字元預算時裁掉舊 turn（注意不能拆散 tool call/result 配對） | `agent.rs` 的 `trim_history` |
| `sanitize_schema` 直接刪 `$ref` | 先把 `$defs` 定義 inline 回去再清理（深度限制防循環） | `src/provider/gemini.rs` |

建議練習（依難度排序）：

1. **在 `.mcp.json` 多接一個 server**（例如 `@modelcontextprotocol/server-memory`），然後把「同名工具先到先贏」改成不會衝突的做法（提示：rs-cli 用 `server__tool` 前綴——宣告時加上去、呼叫時拆回來）
2. **加第二個技能**，感受「加技能不用改程式碼」
3. **讓 `confirm()` 記住「這個工具本次 session 一律允許」**——多一個 `[y/N/a]` 選項，`a` 就把工具名記進一個 `HashSet`，之後同名工具不再問。這是 Claude Code 的 "Always Allow" 的最小版本，也會讓你發現 `confirm()` 需要一個能跨輪保存的狀態
4. **裁 history**：超過字元預算時砍掉最舊的 turn，但**不能拆散 tool call/result 配對**（OpenAI 和 Claude 都會因為 `tool_use` 少了對應的結果而 400）——這題會讓你發現中性型別讓這件事變好寫
5. **看 Claude 的思考過程**：body 加上 `"thinking": {"type": "adaptive", "display": "summarized"}`，然後在解碼器裡把 `thinking_delta` 也印出來（現在只累積不印）

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
