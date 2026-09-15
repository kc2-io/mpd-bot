use crate::config::{Config, Provider};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role { User, Assistant }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message { pub role: Role, pub content: String }

pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(45))
        .pool_max_idle_per_host(1).pool_idle_timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none()).user_agent("mpd-bot/0.1").build()
}

fn request<'a>(config: &'a Config, messages: &[Message]) -> (&'a str, Value) {
    let system = config.system_prompt();
    match config.provider {
        Provider::Openai => ("https://api.openai.com/v1/responses", json!({
            "model":config.model, "instructions":system, "input":messages,
            "max_output_tokens":config.max_output_tokens, "store":false
        })),
        Provider::Anthropic => ("https://api.anthropic.com/v1/messages", json!({
            "model":config.model, "system":system, "messages":messages, "max_tokens":config.max_output_tokens
        })),
        Provider::Openrouter | Provider::Compatible => {
            let mut chat = vec![json!({"role":"system", "content":system})];
            chat.extend(messages.iter().map(|m| json!(m)));
            let endpoint = if config.provider == Provider::Openrouter { "https://openrouter.ai/api/v1/chat/completions" } else { config.endpoint.as_str() };
            (endpoint, json!({"model":config.model, "messages":chat, "max_tokens":config.max_output_tokens}))
        }
    }
}

pub async fn complete(client: &reqwest::Client, config: &Config, key: &str, messages: &[Message]) -> Result<String, String> {
    let (url, body) = request(config, messages);
    let mut request = client.post(url).json(&body);
    if config.provider == Provider::Anthropic {
        request = request.header("x-api-key", key).header("anthropic-version", "2023-06-01");
    } else if !key.is_empty() { request = request.bearer_auth(key); }
    let mut response = request.send().await.map_err(|e| {
        if e.is_timeout() { "Provider timed out after 45 seconds.".to_string() } else { "Could not connect to provider. Check the endpoint and network.".to_string() }
    })?;
    if !response.status().is_success() {
        // Never echo upstream bodies: they can contain prompt text or credentials.
        return Err(match response.status().as_u16() {
            401 | 403 => "Provider rejected authentication. Check the API key and model access.".into(),
            429 => "Provider rate limit or credit limit reached. Try again later.".into(),
            code => format!("Provider returned HTTP {code}. Check the model ID, token budget, and endpoint."),
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "Could not read provider response.")? {
        if bytes.len() + chunk.len() > 262_144 { return Err("Provider response exceeded 256 KiB.".into()); }
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&bytes).map_err(|_| "Provider returned invalid JSON.")?;
    parse(config.provider, value)
}

fn parse(provider: Provider, value: Value) -> Result<String, String> {
    if value.get("error").is_some_and(|v| !v.is_null()) { return Err("Provider reported an error.".into()); }
    let mut parts = Vec::new();
    match provider {
        Provider::Openai => {
            if let Some(items) = value["output"].as_array() {
                for item in items {
                    if item["type"] != "message" { continue; }
                    if let Some(content) = item["content"].as_array() {
                        for block in content { if block["type"] == "output_text" { if let Some(s) = block["text"].as_str() { parts.push(s); } } }
                    }
                }
            }
        }
        Provider::Anthropic => {
            if let Some(content) = value["content"].as_array() {
                for block in content { if block["type"] == "text" { if let Some(s) = block["text"].as_str() { parts.push(s); } } }
            }
        }
        Provider::Openrouter | Provider::Compatible => {
            if let Some(s) = value["choices"][0]["message"]["content"].as_str() { parts.push(s); }
        }
    }
    let text = parts.join(" ").trim().to_string();
    if text.is_empty() { Err("Provider returned no text. The model may have refused or exhausted its reasoning budget; try a larger token limit.".into()) } else { Ok(text) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn response_blocks_exclude_reasoning_and_tools() {
        assert_eq!(parse(Provider::Openai, json!({"output":[{"type":"reasoning","text":"private"},{"type":"message","content":[{"type":"output_text","text":"Hi"},{"type":"output_text","text":"chat"}]}]})).unwrap(), "Hi chat");
        assert_eq!(parse(Provider::Anthropic, json!({"content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"Hello"}]})).unwrap(), "Hello");
        for p in [Provider::Openrouter, Provider::Compatible] { assert_eq!(parse(p, json!({"choices":[{"message":{"content":"Hello"}}]})).unwrap(), "Hello"); }
        assert!(parse(Provider::Openai, json!({"output":[]})).is_err());
        assert!(parse(Provider::Openrouter, json!({"error":{"message":"secret"}})).is_err());
    }
    #[test] fn correct_wire_formats_without_unsupported_temperature() {
        let mut c = Config::default();
        let messages = [Message { role: Role::User, content:"hi".into() }];
        for p in Provider::ALL {
            c.provider = p;
            let (_, body) = request(&c, &messages);
            assert!(body.get("temperature").is_none());
            match p {
                Provider::Openai => { assert_eq!(body["store"], false); assert_eq!(body["input"][0]["role"], "user"); }
                Provider::Anthropic => { assert!(body["system"].is_string()); assert_eq!(body["messages"][0]["role"], "user"); }
                _ => { assert_eq!(body["messages"][0]["role"], "system"); }
            }
        }
    }
}
