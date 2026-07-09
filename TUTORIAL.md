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

**(1) SSE parser** — SSE 是純文字協定，每個事件長這樣：`data: {...json...}\n\n`。因為網路 chunk 可能把事件切在任意位置，要自己緩衝：

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
        self.buf.push_str(&String::from_utf8_lossy(chunk));
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
        self.buf.push_str(&String::from_utf8_lossy(chunk));
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

## Step 5 — 延伸：從 rs-agent 到 rs-cli

到這裡你已經有一個 200 行的完整 agent。rs-cli 做的事就是在這個骨架上疊加工程化的東西，對照著讀：

| rs-agent 的做法 | rs-cli 的做法 | 檔案 |
|---|---|---|
| 寫死 Gemini | `Provider` trait + 三個實作，config 切換 | `src/provider/mod.rs` |
| 訊息用 `serde_json::Value` 硬組 | 內部統一 `Message`/`ContentBlock` 型別，各 provider 自己轉換 | `src/provider/types.rs` |
| 工具寫死在程式裡 | MCP：工具由外部 server 提供，動態聚合 | `src/mcp/manager.rs` |
| main 裡一坨 loop | agent loop 抽成 `Agent::run_turn`，UI 用 callback 解耦 | `src/agent.rs` |
| history 無限長 | 超過字元預算時裁掉舊 turn（注意不能拆散 tool call/result 配對） | `agent.rs` 的 `trim_history` |
| — | Skills 漸進式揭露（system prompt 只放目錄，內容按需讀取） | `src/skills.rs` |

建議練習（依難度排序）：

1. **加第三個工具** `write_file`（含確認），感受加工具有多便宜
2. **加 system prompt**：body 加 `"systemInstruction": {"parts": [{"text": "..."}]}`，給 agent 個性
3. **抽 Provider trait**：定義 `trait Provider { async fn chat(...) }`，先做 Gemini 實作，再加一個 Ollama（OpenAI-compat）實作——做完你就懂為什麼 rs-cli 要有 `types.rs`
4. **接一個 MCP server**：用 `rmcp` crate，把寫死的工具換成外部來的

---

## 常見坑速查

| 症狀 | 原因 |
|---|---|
| `400 Unknown name "$defs" ... parameters` | Gemini 的 tool schema 是受限的 OpenAPI 子集，不接受 `$ref`/`$defs`/`additionalProperties`/`oneOf`（rs-cli 的 `sanitize_schema` 就是在處理這個） |
| 串流看不到逐字效果 | `print!` 之後忘了 `io::stdout().flush()` |
| 模型「失憶」 | 忘了把 model 回覆 push 回 history |
| tool loop 跑不停 | 工具回傳錯誤訊息但格式讓模型誤解，或沒把 `functionResponse` 塞回 history |
| `429 RESOURCE_EXHAUSTED` | 免費額度限流，換 `gemini-2.5-flash-lite` 或稍等 |
