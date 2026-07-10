use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::io::{self, Write};

const MODEL: &str = "gemini-2.5-flash";

fn call_gemini(
    client: &reqwest::blocking::Client,
    api_key: &str,
    history: &[Value],
) -> Result<String> {
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
