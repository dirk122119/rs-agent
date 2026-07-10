use anyhow::{Result, bail};
use serde_json::{Value, json};

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
