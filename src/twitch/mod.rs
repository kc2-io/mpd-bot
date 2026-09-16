//! Direct Twitch: EventSub receives chat; Helix confirms outgoing delivery.
pub mod auth;
mod auth_store;
use crate::{
    application::App,
    config::{Config, valid_login},
    diagnostics::{Event, Field, Level, Subsystem},
    engine::ChatInput,
};
use auth::{AccessSession, AuthError};
use futures_util::{SinkExt, StreamExt, future::BoxFuture};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpStream,
    sync::watch,
    time::{Instant, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const SOCKET: &str = "wss://eventsub.wss.twitch.tv/ws?keepalive_timeout_seconds=30";
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Shared = Arc<App>;
type Seen = VecDeque<(String, Instant)>;
#[derive(Debug)]
enum Error {
    Auth,
    Network,
    Changed,
    RateLimited(Duration),
    Setup(&'static str),
}
#[derive(Clone)]
struct SessionAuth {
    access: AccessSession,
    legacy: bool,
    revision: u64,
}
#[derive(Deserialize)]
struct Account {
    client_id: String,
    login: String,
    user_id: String,
    scopes: Vec<String>,
}
#[derive(Deserialize)]
struct Data<T> {
    data: Vec<T>,
}
#[derive(Deserialize)]
struct User {
    id: String,
}
#[derive(Deserialize)]
struct Delivery {
    is_sent: bool,
    drop_reason: Option<DropReason>,
}
#[derive(Deserialize)]
struct DropReason {
    code: String,
}
#[derive(Deserialize)]
struct Envelope {
    metadata: Metadata,
    payload: Payload,
}
#[derive(Deserialize)]
struct Metadata {
    message_id: String,
    message_type: String,
    subscription_type: Option<String>,
}
#[derive(Deserialize)]
struct Payload {
    session: Option<Session>,
    event: Option<ChatEvent>,
}
#[derive(Deserialize)]
struct Session {
    id: String,
    keepalive_timeout_seconds: Option<u64>,
    reconnect_url: Option<String>,
}
#[derive(Deserialize)]
struct ChatEvent {
    broadcaster_user_id: String,
    chatter_user_id: String,
    chatter_user_login: String,
    message_id: String,
    message: ChatText,
    source_broadcaster_user_id: Option<String>,
}
#[derive(Deserialize)]
struct ChatText {
    text: String,
}

fn auth_error(error: AuthError) -> Error {
    match error {
        AuthError::Network => Error::Network,
        _ => Error::Auth,
    }
}
fn authorized_identity_matches(a: &AccessSession, b: &AccessSession) -> bool {
    a.user_id == b.user_id
        && a.client_id == b.client_id
        && ["user:read:chat", "user:write:chat"]
            .iter()
            .all(|scope| a.scopes.iter().any(|granted| granted == scope))
}

pub async fn run(app: Shared, mut changed: watch::Receiver<u64>) {
    let mut delay = 2u64;
    let mut seen = Seen::new();
    loop {
        let revision = *changed.borrow_and_update();
        let config = app.config.read().await.clone();
        let connected = app.auth.current().await.is_some();
        let legacy_available =
            app.legacy_twitch && !app.secrets.read().await.get("twitch").is_empty();
        if !config.enabled || !config.twitch_enabled || (!connected && !legacy_available) {
            app.set_twitch_status(
                if !config.enabled {
                    "Paused"
                } else if !config.twitch_enabled {
                    "Disabled"
                } else {
                    "Connect Twitch to authorize the bot account"
                }
                .into(),
            );
            if changed.changed().await.is_err() {
                return;
            }
            delay = 2;
            continue;
        }
        app.set_twitch_status("Connecting to Twitch EventSub…".into());
        let started = Instant::now();
        let result = tokio::select! {
            change = changed.changed() => { if change.is_err() { return; } delay = 2; continue; },
            result = session(app.clone(), &config, &mut seen, revision) => result,
        };
        if matches!(result, Err(Error::Changed)) {
            delay = 2;
            continue;
        }
        let setup_error = match &result {
            Err(Error::Auth) => Some("Twitch authorization is no longer valid. Reconnect Twitch."),
            Err(Error::Setup(message)) => Some(*message),
            _ => None,
        };
        if let Some(message) = setup_error {
            app.refresh_auth_view().await;
            app.set_twitch_status(message.into());
            app.diagnostics
                .emit(Level::Warn, Subsystem::Twitch, Event::Disconnected, &[]);
            app.publish_logs();
            if changed.changed().await.is_err() {
                return;
            }
            delay = 2;
        } else {
            if started.elapsed() > Duration::from_secs(60) {
                delay = 2;
            }
            let wait = if let Err(Error::RateLimited(wait)) = result {
                wait
            } else {
                Duration::from_secs(delay)
            };
            app.set_twitch_status(format!(
                "Disconnected. Reconnecting in {}s…",
                wait.as_secs()
            ));
            app.diagnostics
                .emit(Level::Warn, Subsystem::Twitch, Event::Reconnecting, &[]);
            app.publish_logs();
            tokio::select! { change = changed.changed() => { if change.is_err() { return; } delay = 2; }, _ = tokio::time::sleep(wait) => { delay = (delay * 2).min(60); } }
        }
    }
}
fn rate_limit(response: &reqwest::Response) -> Duration {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let seconds = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| {
            response
                .headers()
                .get("ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(|reset| reset.saturating_sub(now))
        })
        .unwrap_or(30)
        .clamp(1, 3600);
    Duration::from_secs(seconds)
}
async fn read_json<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T, Error> {
    match response.status().as_u16() {
        401 => return Err(Error::Auth),
        403 => {
            return Err(Error::Setup(
                "Twitch denied access. Check bot scopes and channel permissions.",
            ));
        }
        400 => {
            return Err(Error::Setup(
                "Twitch rejected the request. Check bot account and channel settings.",
            ));
        }
        409 => {
            return Err(Error::Setup(
                "A Twitch subscription already exists. Close the other bot instance and reconnect.",
            ));
        }
        429 => return Err(Error::RateLimited(rate_limit(&response))),
        200..=299 => {}
        _ => return Err(Error::Network),
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
        if bytes.len() + chunk.len() > 262_144 {
            return Err(Error::Network);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Network)
}
async fn validate_legacy(
    client: &reqwest::Client,
    username: &str,
    token: &str,
) -> Result<AccessSession, Error> {
    let response = client
        .get("https://id.twitch.tv/oauth2/validate")
        .header("Authorization", format!("OAuth {token}"))
        .send()
        .await
        .map_err(|_| Error::Network)?;
    let account: Account = read_json(response).await?;
    if account.login != username
        || !["user:read:chat", "user:write:chat"]
            .iter()
            .all(|s| account.scopes.iter().any(|v| v == s))
    {
        return Err(Error::Auth);
    }
    Ok(AccessSession {
        access_token: token.into(),
        client_id: account.client_id,
        login: account.login,
        user_id: account.user_id,
        scopes: account.scopes,
        generation: 0,
    })
}
async fn current_access(app: &Shared, expected: &SessionAuth) -> Result<AccessSession, Error> {
    if *app.changed.borrow() != expected.revision {
        return Err(Error::Changed);
    }
    let access = if expected.legacy {
        expected.access.clone()
    } else {
        app.auth.current().await.ok_or(Error::Auth)?
    };
    if !authorized_identity_matches(&access, &expected.access) {
        return Err(Error::Auth);
    }
    Ok(access)
}
/// Only explicit HTTP 401 permits one retry. Timeouts, 429 and ambiguous sends never retry.
async fn helix(
    app: &Shared,
    expected: &SessionAuth,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
    query: Option<(&str, &str)>,
) -> Result<reqwest::Response, Error> {
    helix_guarded(app, expected, method, path, body, query, None).await
}
#[allow(clippy::too_many_arguments)]
async fn helix_guarded(
    app: &Shared,
    expected: &SessionAuth,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
    query: Option<(&str, &str)>,
    guard: Option<&crate::chatter_runtime::Admission>,
) -> Result<reqwest::Response, Error> {
    let mut access = current_access(app, expected).await?;
    for attempt in 0..2 {
        if *app.changed.borrow() != expected.revision {
            return Err(Error::Changed);
        }
        let mut request = app
            .client
            .request(
                method.clone(),
                format!("https://api.twitch.tv/helix/{path}"),
            )
            .bearer_auth(&access.access_token)
            .header("Client-Id", &access.client_id)
            .timeout(Duration::from_secs(8));
        if let Some(body) = &body {
            request = request.json(body);
        }
        if let Some(query) = query {
            request = request.query(&[query]);
        }
        if guard.is_some_and(|g| !app.chatters.dispatch(g)) {
            return Err(Error::Changed);
        }
        let response = request.send().await.map_err(|_| Error::Network)?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED || attempt == 1 || expected.legacy
        {
            return Ok(response);
        }
        access = app
            .auth
            .refresh_after_401(access.generation)
            .await
            .map_err(auth_error)?;
        app.refresh_auth_view().await;
        if !authorized_identity_matches(&access, &expected.access) {
            return Err(Error::Auth);
        }
    }
    Err(Error::Auth)
}
fn subscription_body(broadcaster: &str, user: &str, session: &str) -> Value {
    json!({"type":"channel.chat.message","version":"1","condition":{"broadcaster_user_id":broadcaster,"user_id":user},"transport":{"method":"websocket","session_id":session}})
}
async fn subscribe(
    app: Shared,
    account: SessionAuth,
    broadcaster: String,
    session: String,
) -> Result<(), Error> {
    let response = helix(
        &app,
        &account,
        reqwest::Method::POST,
        "eventsub/subscriptions",
        Some(subscription_body(
            &broadcaster,
            &account.access.user_id,
            &session,
        )),
        None,
    )
    .await?;
    let _: Value = read_json(response).await?;
    Ok(())
}
async fn connect(url: String) -> Result<(Socket, Session), Error> {
    timeout(Duration::from_secs(15), async move {
        let config = WebSocketConfig::default()
            .max_message_size(Some(262_144))
            .max_frame_size(Some(262_144));
        let (mut socket, _) = connect_async_with_config(url, Some(config), false)
            .await
            .map_err(|_| Error::Network)?;
        loop {
            match socket
                .next()
                .await
                .ok_or(Error::Network)?
                .map_err(|_| Error::Network)?
            {
                Message::Text(text) => {
                    let welcome: Envelope =
                        serde_json::from_str(&text).map_err(|_| Error::Network)?;
                    if welcome.metadata.message_type != "session_welcome" {
                        return Err(Error::Network);
                    }
                    return Ok((socket, welcome.payload.session.ok_or(Error::Network)?));
                }
                Message::Ping(data) => socket
                    .send(Message::Pong(data))
                    .await
                    .map_err(|_| Error::Network)?,
                _ => return Err(Error::Network),
            }
        }
    })
    .await
    .map_err(|_| Error::Network)?
}
fn keepalive(session: &Session) -> Duration {
    Duration::from_secs(
        session
            .keepalive_timeout_seconds
            .unwrap_or(30)
            .clamp(10, 600),
    )
}
fn reconnect_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "wss"
            && u.host_str() == Some("eventsub.wss.twitch.tv")
            && u.username().is_empty()
            && u.password().is_none()
            && u.port().is_none_or(|p| p == 443)
            && u.fragment().is_none()
    })
}
async fn session(
    app: Shared,
    config: &Config,
    seen: &mut Seen,
    revision: u64,
) -> Result<(), Error> {
    let account = if let Some(access) = app.auth.current().await {
        SessionAuth {
            access,
            legacy: false,
            revision,
        }
    } else {
        if !app.legacy_twitch {
            return Err(Error::Auth);
        }
        let token = app.secrets.read().await.get("twitch");
        SessionAuth {
            access: validate_legacy(&app.client, &config.twitch_username, &token).await?,
            legacy: true,
            revision,
        }
    };
    let response = helix(
        &app,
        &account,
        reqwest::Method::GET,
        "users",
        None,
        Some(("login", &config.twitch_channel)),
    )
    .await?;
    let users: Data<User> = read_json(response).await?;
    let broadcaster = users
        .data
        .into_iter()
        .next()
        .ok_or(Error::Setup(
            "Twitch channel not found. Check the channel login.",
        ))?
        .id;
    let (mut socket, welcome) = connect(SOCKET.into()).await?;
    let mut keepalive = keepalive(&welcome);
    let mut deadline = Instant::now() + keepalive;
    let mut subscribing: Option<BoxFuture<'static, Result<(), Error>>> = Some(Box::pin(subscribe(
        app.clone(),
        account.clone(),
        broadcaster.clone(),
        welcome.id,
    )));
    let mut migrating: Option<BoxFuture<'static, Result<(Socket, Session), Error>>> = None;
    let mut pending: Option<BoxFuture<'static, Result<Option<Duration>, Error>>> = None;
    let mut validating: Option<BoxFuture<'static, Result<(), Error>>> = None;
    let mut validation = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(3600),
        Duration::from_secs(3600),
    );
    let mut send_after = Instant::now();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return Err(Error::Network),
            result = async { subscribing.as_mut().unwrap().await }, if subscribing.is_some() => {
                subscribing = None; result?;
                app.set_twitch_status(format!("EventSub connected · #{} · {}", config.twitch_channel, account.access.login));
                app.diagnostics.emit(Level::Info, Subsystem::Twitch, Event::Connected, &[]);
                app.publish_logs();
            },
            result = async { migrating.as_mut().unwrap().await }, if migrating.is_some() => {
                migrating = None; let (replacement, welcome) = result?;
                // Continue receiving on the old socket until replacement welcome. Subscriptions transfer.
                socket = replacement; keepalive = crate::twitch::keepalive(&welcome); deadline = Instant::now() + keepalive;
            },
            _ = validation.tick(), if account.legacy && validating.is_none() => {
                let app = app.clone(); let account = account.clone();
                validating = Some(Box::pin(async move {
                    validate_legacy(&app.client, &account.access.login, &account.access.access_token).await?;
                    Ok(())
                }));
            },
            result = async { validating.as_mut().unwrap().await }, if validating.is_some() => { validating = None; result?; },
            result = async { pending.as_mut().unwrap().await }, if pending.is_some() => {
                pending = None; if let Some(wait) = result? { send_after = Instant::now() + wait; }
            },
            frame = socket.next() => {
                match frame.ok_or(Error::Network)?.map_err(|_|Error::Network)? {
                    Message::Text(text) => {
                        let envelope: Envelope = serde_json::from_str(&text).map_err(|_|Error::Network)?;
                        match envelope.metadata.message_type.as_str() {
                            "session_keepalive" => { deadline = Instant::now() + keepalive; },
                            "session_reconnect" if migrating.is_none() => {
                                let url = envelope.payload.session.and_then(|s|s.reconnect_url).ok_or(Error::Network)?;
                                if !reconnect_url(&url) { return Err(Error::Network); } migrating = Some(Box::pin(connect(url)));
                            },
                            "revocation" => return Err(Error::Setup("Twitch revoked the chat subscription. Reconnect Twitch to authorize again.")),
                            "notification" => {
                                deadline = Instant::now() + keepalive;
                                if envelope.metadata.subscription_type.as_deref() != Some("channel.chat.message") { continue; }
                                let event = envelope.payload.event.ok_or(Error::Network)?;
                                if !eligible(&event, &broadcaster, &account.access.user_id)
                                    || duplicate(seen, &envelope.metadata.message_id) { continue; }
                                app.chatters.observe(&event.chatter_user_id, &event.chatter_user_login, &broadcaster);
                                if !app.chatters.allowed(&event.chatter_user_id,&event.chatter_user_login)
                                    || config.excluded_users.iter().any(|login|login.eq_ignore_ascii_case(&event.chatter_user_login))
                                    || pending.is_some() || Instant::now() < send_after { continue; }
                                let Some(prompt) = trigger(&event.message.text, config, &account.access.login, || fastrand::u32(0..100)) else { continue; };
                                let input = ChatInput { platform: "twitch".into(), channel: broadcaster.clone(), user: event.chatter_user_id.clone(), message: prompt.into() };
                                let app = app.clone(); let account = account.clone(); let broadcaster = broadcaster.clone();
                                pending = Some(Box::pin(async move {
                                    let Ok(Some(generated)) = app.generate_for(input, false, &event.chatter_user_login).await else { return Ok(None); };
                                    if !app.is_current(&generated) { return Ok(None); }
                                    let message = outgoing(&event.chatter_user_login, &generated.reply);
                                    let response = helix_guarded(&app, &account, reqwest::Method::POST, "chat/messages", Some(json!({"broadcaster_id": broadcaster,"sender_id": account.access.user_id,"message":message,"reply_parent_message_id":event.message_id})), None, generated.chatter.as_deref()).await;
                                    let delivery = match response { Ok(response) => read_json::<Data<Delivery>>(response).await, Err(error) => Err(error) };
                                    match delivery {
                                        Ok(data) if data.data.first().is_some_and(|item|item.is_sent) => {
                                            app.commit(generated).await;

                                        },
                                        Ok(data) => {
                                            app.set_twitch_status(drop_summary(data.data.first().and_then(|item|item.drop_reason.as_ref()).map(|reason|reason.code.as_str())).into());
                                            app.diagnostics.emit(Level::Warn, Subsystem::Twitch, Event::ReplySkipped, &[]);
                                            app.publish_logs();
                                        },
                                        Err(Error::Auth) => return Err(Error::Auth),
                                        Err(Error::Changed) if !app.is_current(&generated) && *app.changed.borrow() == account.revision => return Ok(None),
                                        Err(Error::Changed) => return Err(Error::Changed),
                                        Err(Error::RateLimited(wait)) => {
                                            app.set_twitch_status("Twitch rate limit reached. Replies are paused briefly.".into());
                                            app.diagnostics.emit(Level::Warn, Subsystem::Twitch, Event::ReplySkipped, &[Field::HttpStatus(429)]);
                                            app.publish_logs();
                                            return Ok(Some(wait));
                                        },
                                        Err(Error::Setup(message)) => {
                                            app.set_twitch_status(message.into());
                                            app.diagnostics.emit(Level::Warn, Subsystem::Twitch, Event::ReplySkipped, &[]);
                                            app.publish_logs();
                                        },
                                        Err(Error::Network) => {
                                            app.set_twitch_status("Twitch did not confirm delivery. The reply was not retried.".into());
                                            app.diagnostics.emit(Level::Warn, Subsystem::Twitch, Event::ReplySkipped, &[]);
                                            app.publish_logs();
                                        },
                                    }
                                    Ok(None)
                                }));
                            }, _ => {},
                        }
                    },
                    Message::Ping(bytes) => { timeout(Duration::from_secs(5), socket.send(Message::Pong(bytes))).await.map_err(|_|Error::Network)?.map_err(|_|Error::Network)?; },
                    Message::Close(_) => return Err(Error::Network), _ => {},
                }
            }
        }
    }
}
fn drop_summary(code: Option<&str>) -> &'static str {
    match code {
        Some("automod_held") => "Twitch held the reply for AutoMod review.",
        Some("banned" | "banned_from_channel") => {
            "Twitch rejected the reply because the bot is banned."
        }
        Some("timed_out" | "user_timed_out") => {
            "Twitch rejected the reply because the bot is timed out."
        }
        Some("channel_slow_mode") => "Twitch rejected the reply because slow mode is active.",
        Some("channel_followers_only") => {
            "Twitch rejected the reply because follower-only mode is active."
        }
        Some("channel_subscribers_only") => {
            "Twitch rejected the reply because subscriber-only mode is active."
        }
        Some("channel_emote_only") => {
            "Twitch rejected the reply because emote-only mode is active."
        }
        _ => "Twitch rejected the reply. Check channel permissions and chat restrictions.",
    }
}
fn outgoing(login: &str, reply: &str) -> String {
    format!("@{login} {reply}").chars().take(500).collect()
}
fn eligible(event: &ChatEvent, broadcaster: &str, bot_id: &str) -> bool {
    event.broadcaster_user_id == broadcaster
        && event.chatter_user_id != bot_id
        && valid_login(&event.chatter_user_login)
        && !event.chatter_user_id.is_empty()
        && event.chatter_user_id.len() <= 128
        && event
            .source_broadcaster_user_id
            .as_deref()
            .is_none_or(|source| source == broadcaster)
        && !event.message_id.is_empty()
        && event.message_id.len() <= 128
}
fn duplicate(seen: &mut Seen, id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return true;
    }
    let now = Instant::now();
    while seen
        .front()
        .is_some_and(|(_, at)| now.duration_since(*at) >= Duration::from_secs(600))
    {
        seen.pop_front();
    }
    if seen.iter().any(|(value, _)| value == id) {
        return true;
    }
    if seen.len() >= 512 {
        seen.pop_front();
    }
    seen.push_back((id.into(), now));
    false
}
/// Sample only ordinary chat, once per eligible, deduplicated event. Explicit
/// triggers bypass sampling but still use the normal admission/cooldown checks.
fn trigger<'a>(
    text: &'a str,
    config: &Config,
    login: &str,
    sample: impl FnOnce() -> u32,
) -> Option<&'a str> {
    let text = text.trim();
    let first = text.split_whitespace().next()?;
    let is_mention = first
        .trim_end_matches([',', ':'])
        .eq_ignore_ascii_case(&format!("@{login}"));
    let is_command = !config.command.is_empty() && first.eq_ignore_ascii_case(&config.command);
    // Disabled direct triggers must not re-enter via random selection.
    if (is_mention && !config.respond_to_mentions) || (is_command && !config.command_enabled) {
        return None;
    }
    if (config.command_enabled && is_command) || (config.respond_to_mentions && is_mention) {
        let message = text[first.len()..].trim();
        return (!message.is_empty()).then_some(message);
    }
    // Leave other bots' commands alone, including !ai when its toggle is off.
    if text.starts_with('!') || config.random_reply_percent == 0 {
        return None;
    }
    (sample() < config.random_reply_percent).then_some(text)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refreshed_session_requires_same_identity_and_chat_scopes() {
        let expected = AccessSession {
            access_token: "test".into(),
            client_id: "app".into(),
            login: "bot".into(),
            user_id: "1".into(),
            scopes: vec!["user:read:chat".into(), "user:write:chat".into()],
            generation: 1,
        };
        let mut refreshed = expected.clone();
        refreshed.generation = 2;
        assert!(authorized_identity_matches(&refreshed, &expected));
        refreshed.scopes.pop();
        assert!(!authorized_identity_matches(&refreshed, &expected));
        refreshed = expected.clone();
        refreshed.user_id = "2".into();
        assert!(!authorized_identity_matches(&refreshed, &expected));
        refreshed = expected.clone();
        refreshed.client_id = "another-app".into();
        assert!(!authorized_identity_matches(&refreshed, &expected));
    }
    #[test]
    fn eventsub_json_and_subscription_contract() {
        let envelope: Envelope = serde_json::from_value(json!({"metadata":{"message_id":"envelope-id","message_type":"notification","subscription_type":"channel.chat.message"},"payload":{"event":{"broadcaster_user_id":"1","chatter_user_id":"2","chatter_user_login":"viewer","message_id":"chat-id","message":{"text":"!ai hi","fragments":[]},"source_broadcaster_user_id":null}}})).unwrap();
        assert_eq!(envelope.metadata.message_id, "envelope-id");
        let mut event = envelope.payload.event.unwrap();
        assert!(eligible(&event, "1", "3"));
        assert!(!eligible(&event, "1", "2"));
        event.source_broadcaster_user_id = Some("other".into());
        assert!(!eligible(&event, "1", "3"));
        let body = subscription_body("1", "2", "session");
        assert_eq!(body["transport"]["method"], "websocket");
        assert_eq!(body["condition"]["user_id"], "2");
    }
    #[test]
    fn duplicate_window_is_bounded_and_expires() {
        let mut seen = Seen::new();
        assert!(!duplicate(&mut seen, "same"));
        assert!(duplicate(&mut seen, "same"));
        for i in 0..600 {
            duplicate(&mut seen, &i.to_string());
        }
        assert_eq!(seen.len(), 512);
        seen.clear();
        seen.push_back(("expired".into(), Instant::now() - Duration::from_secs(601)));
        assert!(!duplicate(&mut seen, "expired"));
        assert!(duplicate(&mut seen, ""));
    }
    #[test]
    fn reconnect_restricts_destination() {
        assert!(reconnect_url("wss://eventsub.wss.twitch.tv/ws?session=abc"));
        assert!(!reconnect_url("wss://evil.example/ws"));
        assert!(!reconnect_url("ws://eventsub.wss.twitch.tv/ws"));
    }
    #[test]
    fn explicit_triggers_are_optional_and_bypass_random_sampling() {
        let mut config = Config {
            command_enabled: true,
            random_reply_percent: 0,
            ..Config::default()
        };
        let no_sample = || panic!("explicit trigger must not sample");
        assert_eq!(
            trigger("!AI hello", &config, "bot", no_sample),
            Some("hello")
        );
        assert_eq!(trigger("@Bot, hi", &config, "bot", no_sample), Some("hi"));
        for text in [
            "!air hello",
            "hello !ai",
            "!ai",
            "@botother hi",
            "@bot",
            " ",
        ] {
            assert_eq!(trigger(text, &config, "bot", no_sample), None);
        }
        config.command_enabled = false;
        config.random_reply_percent = 100;
        assert_eq!(trigger("!ai hello", &config, "bot", no_sample), None);
        assert_eq!(trigger("!song", &config, "bot", no_sample), None);
        assert_eq!(
            trigger("@bot: hello", &config, "bot", no_sample),
            Some("hello")
        );
        config.command = "ask".into();
        assert_eq!(trigger("ask hello", &config, "bot", no_sample), None);
    }
    #[test]
    fn disabled_mentions_do_not_fall_back_to_random_replies() {
        let config = Config {
            respond_to_mentions: false,
            command_enabled: true,
            random_reply_percent: 100,
            ..Config::default()
        };
        let no_sample = || panic!("direct trigger must not sample");
        for text in ["@bot hi", "@BOT, hi", "@Bot: hello", "@bot"] {
            assert_eq!(trigger(text, &config, "bot", no_sample), None);
        }
        assert_eq!(trigger("!ai hi", &config, "bot", no_sample), Some("hi"));
        assert_eq!(
            trigger("hello chat", &config, "bot", || 99),
            Some("hello chat")
        );
    }
    #[test]
    fn random_replies_cover_percentage_boundaries_and_keep_message_intact() {
        for percent in [0, 1, 10, 50, 99, 100] {
            let config = Config {
                random_reply_percent: percent,
                ..Config::default()
            };
            let selected = (0..100)
                .filter(|roll| {
                    let result = trigger("  This stream is fun  ", &config, "bot", || *roll);
                    if result.is_some() {
                        assert_eq!(result, Some("This stream is fun"));
                    }
                    result.is_some()
                })
                .count();
            assert_eq!(selected, percent as usize);
        }
    }
    #[test]
    fn outgoing_limit_includes_mention_and_drop_reason_is_curated() {
        assert_eq!(outgoing("viewer", &"🙂".repeat(600)).chars().count(), 500);
        assert!(!drop_summary(Some("SECRET_SENTINEL")).contains("SECRET_SENTINEL"));
    }
}
