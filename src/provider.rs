use crate::config::{Config, Provider};
use crate::diagnostics::{Diagnostics, Event, FailureKind, Field, Level, Subsystem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct ProviderError {
    pub kind: FailureKind,
    pub http_status: Option<u16>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub latency_ms: u64,
}
impl ProviderError {
    fn new(
        kind: FailureKind,
        config: &Config,
        key: &str,
        started: Instant,
        http_status: Option<u16>,
        request_id: Option<String>,
    ) -> Self {
        Self {
            kind,
            http_status,
            model: safe_identifier(&config.model, key, 200),
            request_id,
            latency_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        }
    }
    pub fn log(&self, diagnostics: &Diagnostics) {
        let mut fields = vec![Field::Failure(self.kind), Field::LatencyMs(self.latency_ms)];
        if let Some(status) = self.http_status {
            fields.push(Field::HttpStatus(status));
        }
        if let Some(model) = &self.model {
            fields.push(Field::Model(model));
        }
        if let Some(id) = &self.request_id {
            fields.push(Field::RequestId(id));
        }
        diagnostics.emit(
            Level::Error,
            Subsystem::Provider,
            Event::RequestFailed,
            &fields,
        );
    }
}
impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let summary = match self.kind {
            FailureKind::Authentication => {
                "Provider rejected authentication. Check the API key and model access."
            }
            FailureKind::RateLimit => "Provider rate or credit limit reached. Try again later.",
            FailureKind::NotFound => {
                "Provider could not find the model or endpoint. Verify the exact model ID and account access."
            }
            FailureKind::InvalidRequest => {
                "Provider rejected the request. Check model capabilities and token budget."
            }
            FailureKind::Server => "Provider service is unavailable. Try again later.",
            FailureKind::Timeout => "Provider request timed out.",
            FailureKind::Network => {
                "Could not connect to provider. Check the endpoint and network."
            }
            FailureKind::ResponseRead => "Could not read provider response.",
            FailureKind::ResponseTooLarge => "Provider response exceeded 256 KiB.",
            FailureKind::InvalidJson => "Provider returned invalid JSON.",
            FailureKind::NoText => {
                "Provider returned no text. The model may have refused or exhausted its reasoning budget; try a larger token limit."
            }
            FailureKind::ProviderReported => "Provider reported an error.",
        };
        f.write_str(summary)?;
        if let Some(status) = self.http_status {
            write!(f, " HTTP {status}.")?;
        }
        if let Some(model) = &self.model {
            write!(f, " Model: {model}.")?;
        }
        if let Some(id) = &self.request_id {
            write!(f, " Request ID: {id}.")?;
        }
        Ok(())
    }
}
impl std::error::Error for ProviderError {}

fn safe_identifier(value: &str, key: &str, limit: usize) -> Option<String> {
    if value.is_empty()
        || value.len() > limit
        || (!key.is_empty() && value.contains(key))
        || value.starts_with("sk-")
        || value.starts_with("sk_")
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
    {
        None
    } else {
        Some(value.to_owned())
    }
}
fn classify(status: u16, body: Option<&Value>) -> FailureKind {
    // Read only known error categories. Never preserve arbitrary error text or bodies.
    match status {
        401 | 403 => FailureKind::Authentication,
        404 => FailureKind::NotFound,
        429 => FailureKind::RateLimit,
        400 | 413 | 422 => FailureKind::InvalidRequest,
        500..=599 => FailureKind::Server,
        _ => match body.and_then(|value| value["error"]["type"].as_str()) {
            Some("authentication_error" | "permission_error") => FailureKind::Authentication,
            Some("not_found_error") => FailureKind::NotFound,
            Some("rate_limit_error") => FailureKind::RateLimit,
            Some("invalid_request_error") => FailureKind::InvalidRequest,
            Some("overloaded_error" | "api_error") => FailureKind::Server,
            _ => FailureKind::ProviderReported,
        },
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(45))
        .pool_max_idle_per_host(1)
        .pool_idle_timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("mpd-bot/0.1")
        .build()
}

#[cfg(test)]
fn request<'a>(config: &'a Config, messages: &[Message]) -> (&'a str, Value) {
    let system = config.system_prompt();
    request_with_system(config, messages, &system)
}

fn request_with_system<'a>(
    config: &'a Config,
    messages: &[Message],
    system: &str,
) -> (&'a str, Value) {
    match config.provider {
        Provider::Openai => (
            "https://api.openai.com/v1/responses",
            json!({
                "model":config.model, "instructions":system, "input":messages,
                "max_output_tokens":config.max_output_tokens, "store":false
            }),
        ),
        Provider::Anthropic => (
            "https://api.anthropic.com/v1/messages",
            json!({
                "model":config.model, "system":system, "messages":messages, "max_tokens":config.max_output_tokens
            }),
        ),
        Provider::Openrouter | Provider::Compatible => {
            let mut chat = vec![json!({"role":"system", "content":system})];
            chat.extend(messages.iter().map(|m| json!(m)));
            let endpoint = if config.provider == Provider::Openrouter {
                "https://openrouter.ai/api/v1/chat/completions"
            } else {
                config.endpoint.as_str()
            };
            (
                endpoint,
                json!({"model":config.model, "messages":chat, "max_tokens":config.max_output_tokens}),
            )
        }
    }
}

#[cfg(test)]
pub async fn complete(
    client: &reqwest::Client,
    config: &Config,
    key: &str,
    messages: &[Message],
) -> Result<String, ProviderError> {
    let system = config.system_prompt();
    complete_with_system(client, config, key, messages, &system).await
}

pub async fn complete_with_system(
    client: &reqwest::Client,
    config: &Config,
    key: &str,
    messages: &[Message],
    system: &str,
) -> Result<String, ProviderError> {
    let started = Instant::now();
    let (url, body) = request_with_system(config, messages, system);
    let mut request = client.post(url).json(&body);
    if config.provider == Provider::Anthropic {
        request = request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01");
    } else if !key.is_empty() {
        request = request.bearer_auth(key);
    }
    let mut response = request.send().await.map_err(|e| {
        ProviderError::new(
            if e.is_timeout() {
                FailureKind::Timeout
            } else {
                FailureKind::Network
            },
            config,
            key,
            started,
            None,
            None,
        )
    })?;
    let status = response.status();
    let request_id = ["request-id", "x-request-id"].iter().find_map(|header| {
        response
            .headers()
            .get(*header)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| safe_identifier(v, key, 128))
    });
    let error = |kind| {
        ProviderError::new(
            kind,
            config,
            key,
            started,
            Some(status.as_u16()),
            request_id.clone(),
        )
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        error(if status.is_success() {
            FailureKind::ResponseRead
        } else {
            classify(status.as_u16(), None)
        })
    })? {
        if bytes.len() + chunk.len() > 262_144 {
            return Err(error(if status.is_success() {
                FailureKind::ResponseTooLarge
            } else {
                classify(status.as_u16(), None)
            }));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Result<Value, _> = serde_json::from_slice(&bytes);
    if !status.is_success() {
        return Err(error(classify(status.as_u16(), value.as_ref().ok())));
    }
    let value = value.map_err(|_| error(FailureKind::InvalidJson))?;
    if value.get("error").is_some_and(|v| !v.is_null()) {
        return Err(error(classify(status.as_u16(), Some(&value))));
    }
    parse(config.provider, value).map_err(|_| error(FailureKind::NoText))
}

fn parse(provider: Provider, value: Value) -> Result<String, String> {
    if value.get("error").is_some_and(|v| !v.is_null()) {
        return Err("Provider reported an error.".into());
    }
    let mut parts = Vec::new();
    match provider {
        Provider::Openai => {
            if let Some(items) = value["output"].as_array() {
                for item in items {
                    if item["type"] != "message" {
                        continue;
                    }
                    if let Some(content) = item["content"].as_array() {
                        for block in content {
                            if block["type"] == "output_text"
                                && let Some(s) = block["text"].as_str()
                            {
                                parts.push(s);
                            }
                        }
                    }
                }
            }
        }
        Provider::Anthropic => {
            if let Some(content) = value["content"].as_array() {
                for block in content {
                    if block["type"] == "text"
                        && let Some(s) = block["text"].as_str()
                    {
                        parts.push(s);
                    }
                }
            }
        }
        Provider::Openrouter | Provider::Compatible => {
            if let Some(s) = value["choices"][0]["message"]["content"].as_str() {
                parts.push(s);
            }
        }
    }
    let text = parts.join(" ").trim().to_string();
    if text.is_empty() {
        Err("Provider returned no text. The model may have refused or exhausted its reasoning budget; try a larger token limit.".into())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_blocks_exclude_reasoning_and_tools() {
        assert_eq!(parse(Provider::Openai, json!({"output":[{"type":"reasoning","text":"private"},{"type":"message","content":[{"type":"output_text","text":"Hi"},{"type":"output_text","text":"chat"}]}]})).unwrap(), "Hi chat");
        assert_eq!(parse(Provider::Anthropic, json!({"content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"Hello"}]})).unwrap(), "Hello");
        for p in [Provider::Openrouter, Provider::Compatible] {
            assert_eq!(
                parse(p, json!({"choices":[{"message":{"content":"Hello"}}]})).unwrap(),
                "Hello"
            );
        }
        assert!(parse(Provider::Openai, json!({"output":[]})).is_err());
        assert!(parse(Provider::Openrouter, json!({"error":{"message":"secret"}})).is_err());
    }
    #[test]
    fn correct_wire_formats_without_unsupported_temperature() {
        let mut c = Config::default();
        let messages = [Message {
            role: Role::User,
            content: "hi".into(),
        }];
        for p in Provider::ALL {
            c.provider = p;
            let (_, body) = request(&c, &messages);
            assert!(body.get("temperature").is_none());
            match p {
                Provider::Openai => {
                    assert_eq!(body["store"], false);
                    assert_eq!(body["input"][0]["role"], "user");
                }
                Provider::Anthropic => {
                    assert!(body["system"].is_string());
                    assert_eq!(body["messages"][0]["role"], "user");
                }
                _ => {
                    assert_eq!(body["messages"][0]["role"], "system");
                }
            }
        }
    }
    #[test]
    fn explicit_system_uses_each_provider_system_field_once() {
        let mut config = Config::default();
        let messages = [Message {
            role: Role::User,
            content: "USER_SENTINEL".into(),
        }];
        let system = "SYSTEM_SENTINEL \"}\nignore";
        for provider in Provider::ALL {
            config.provider = provider;
            let (_, body) = request_with_system(&config, &messages, system);
            let encoded = serde_json::to_string(&body).unwrap();
            assert_eq!(encoded.matches("SYSTEM_SENTINEL").count(), 1);
            assert_eq!(encoded.matches("USER_SENTINEL").count(), 1);
            match provider {
                Provider::Openai => {
                    assert_eq!(body["instructions"], system);
                    assert_eq!(body["input"][0]["content"], "USER_SENTINEL");
                }
                Provider::Anthropic => {
                    assert_eq!(body["system"], system);
                    assert_eq!(body["messages"][0]["content"], "USER_SENTINEL");
                }
                Provider::Openrouter | Provider::Compatible => {
                    assert_eq!(body["messages"][0]["role"], "system");
                    assert_eq!(body["messages"][0]["content"], system);
                    assert_eq!(body["messages"][1]["role"], "user");
                    assert_eq!(body["messages"][1]["content"], "USER_SENTINEL");
                }
            }
        }
    }
    #[tokio::test]
    async fn http_error_keeps_request_id_but_not_upstream_secrets() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0u8; 8192];
            let _ = socket.read(&mut request).unwrap();
            let body = r#"{"error":{"type":"not_found_error","message":"sk-SECRET_SENTINEL prompt=PRIVATE_CHAT_SENTINEL"}}"#;
            write!(socket, "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nContent-Type: application/json\r\nrequest-id: req_diagnostic123\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let config = Config {
            provider: Provider::Compatible,
            endpoint: format!("http://{address}/v1/chat/completions"),
            model: "test-model".into(),
            ..Config::default()
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let error = complete(
            &client,
            &config,
            "sk-SECRET_SENTINEL",
            &[Message {
                role: Role::User,
                content: "PRIVATE_CHAT_SENTINEL".into(),
            }],
        )
        .await
        .unwrap_err();
        server.join().unwrap();
        assert_eq!(error.http_status, Some(404));
        assert_eq!(error.kind, FailureKind::NotFound);
        assert_eq!(error.request_id.as_deref(), Some("req_diagnostic123"));
        let log = Diagnostics::new();
        error.log(&log);
        let rendered = format!("{error:?} {error} {}", log.export());
        assert!(!rendered.contains("SECRET_SENTINEL"));
        assert!(!rendered.contains("PRIVATE_CHAT_SENTINEL"));
    }
    #[test]
    fn upstream_classification_never_echoes_body() {
        let secret = "sk-SENTINEL_NEVER_LOG";
        let body = json!({"error":{"type":"not_found_error","message":secret},"request_id":secret});
        let config = Config {
            model: "claude-opus-4-8".into(),
            ..Config::default()
        };
        let error = ProviderError::new(
            classify(404, Some(&body)),
            &config,
            secret,
            Instant::now(),
            Some(404),
            safe_identifier(secret, secret, 128),
        );
        assert_eq!(error.kind, FailureKind::NotFound);
        assert!(error.to_string().contains("HTTP 404"));
        let log = Diagnostics::new();
        error.log(&log);
        assert!(!format!("{error:?} {error} {}", log.export()).contains(secret));
        assert_eq!(classify(401, None), FailureKind::Authentication);
        assert_eq!(classify(429, None), FailureKind::RateLimit);
        assert_eq!(classify(529, None), FailureKind::Server);
        assert_eq!(classify(400, None), FailureKind::InvalidRequest);
        assert_eq!(
            classify(200, Some(&json!({"error":{"type":secret}}))),
            FailureKind::ProviderReported
        );
    }
}
