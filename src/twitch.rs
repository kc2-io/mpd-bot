//! Direct Twitch: EventSub WebSockets receive chat; Helix sends replies.
use crate::{config::{Config, valid_login}, engine::{ChatInput, ChatOutput}, server::{self, Shared}};
use futures_util::{SinkExt, StreamExt, future::BoxFuture};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use std::{collections::VecDeque, time::Duration};
use tokio::{net::TcpStream, sync::watch, time::{Instant, timeout}};
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream, tungstenite::{Message, protocol::WebSocketConfig}};

const SOCKET: &str = "wss://eventsub.wss.twitch.tv/ws?keepalive_timeout_seconds=30";
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
#[derive(Debug)]
enum Error { Auth, Network, Setup(&'static str) }
#[derive(Clone, Deserialize)]
struct Account { client_id: String, login: String, user_id: String, scopes: Vec<String> }
#[derive(Deserialize)]
struct Data<T> { data: Vec<T> }
#[derive(Deserialize)]
struct User { id: String }
#[derive(Deserialize)]
struct Delivery { is_sent: bool }
#[derive(Deserialize)]
struct Envelope { metadata: Metadata, payload: Payload }
#[derive(Deserialize)]
struct Metadata { message_type: String, subscription_type: Option<String> }
#[derive(Deserialize)]
struct Payload { session: Option<Session>, event: Option<ChatEvent> }
#[derive(Deserialize)]
struct Session { id: String, keepalive_timeout_seconds: Option<u64>, reconnect_url: Option<String> }
#[derive(Deserialize)]
struct ChatEvent {
    broadcaster_user_id: String, chatter_user_id: String, chatter_user_login: String,
    message_id: String, message: ChatText, source_broadcaster_user_id: Option<String>,
}
#[derive(Deserialize)]
struct ChatText { text: String }

pub async fn run(app: Shared, mut changed: watch::Receiver<u64>) {
    let mut delay=2u64; let mut seen=VecDeque::new();
    loop {
        changed.borrow_and_update();
        let config=app.config.read().await.clone(); let token=app.secrets.read().await.get("twitch");
        if !config.enabled || !config.twitch_enabled || token.is_empty() {
            *app.twitch_status.write().await=if !config.enabled { "Paused" } else if !config.twitch_enabled { "Disabled" } else { "Add a Twitch access token" }.into();
            if changed.changed().await.is_err() { return; } delay=2; continue;
        }
        *app.twitch_status.write().await="Connecting to Twitch EventSub…".into();
        let started=Instant::now();
        let result=tokio::select! {
            _ = changed.changed() => { delay=2; continue; },
            result = session(app.clone(),&config,&token,&mut seen) => result,
        };
        let setup_error=match result {
            Err(Error::Auth) => Some("Twitch authentication failed. Use a token for the bot login with user:read:chat and user:write:chat scopes. Save settings to retry."),
            Err(Error::Setup(message)) => Some(message), _ => None,
        };
        if let Some(message)=setup_error {
            *app.twitch_status.write().await=message.into();
            if changed.changed().await.is_err() { return; } delay=2;
        } else {
            if started.elapsed()>Duration::from_secs(60) { delay=2; }
            *app.twitch_status.write().await=format!("Disconnected. Reconnecting in {delay}s…");
            tokio::select! { _ = changed.changed() => { delay=2; }, _ = tokio::time::sleep(Duration::from_secs(delay)) => { delay=(delay*2).min(60); } }
        }
    }
}
async fn read_json<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T,Error> {
    match response.status().as_u16() {
        401 => return Err(Error::Auth),
        403 => return Err(Error::Setup("Twitch denied access. Check the token scopes and the bot's channel permissions.")),
        400 => return Err(Error::Setup("Twitch rejected the request. Check the bot account and channel settings.")),
        200..=299 => {}, _ => return Err(Error::Network),
    }
    let mut bytes=Vec::new();
    while let Some(chunk)=response.chunk().await.map_err(|_|Error::Network)? {
        if bytes.len()+chunk.len()>262_144 { return Err(Error::Network); } bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_|Error::Network)
}
async fn validate_token(client: &reqwest::Client, username: &str, token: &str) -> Result<Account,Error> {
    let response=client.get("https://id.twitch.tv/oauth2/validate").header("Authorization",format!("OAuth {token}")).send().await.map_err(|_|Error::Network)?;
    let account: Account=read_json(response).await?;
    if account.login!=username || !["user:read:chat","user:write:chat"].iter().all(|s|account.scopes.iter().any(|v|v==s)) { return Err(Error::Auth); } Ok(account)
}
fn helix(client: &reqwest::Client, method: reqwest::Method, path: &str, token: &str, account: &Account) -> reqwest::RequestBuilder {
    client.request(method,format!("https://api.twitch.tv/helix/{path}")).bearer_auth(token).header("Client-Id",&account.client_id)
}
fn subscription_body(broadcaster: &str, user: &str, session: &str) -> serde_json::Value {
    json!({"type":"channel.chat.message","version":"1","condition":{"broadcaster_user_id":broadcaster,"user_id":user},"transport":{"method":"websocket","session_id":session}})
}
async fn subscribe(client: reqwest::Client, account: Account, token: String, broadcaster: String, session: String) -> Result<(),Error> {
    let response=helix(&client,reqwest::Method::POST,"eventsub/subscriptions",&token,&account).timeout(Duration::from_secs(8))
        .json(&subscription_body(&broadcaster,&account.user_id,&session)).send().await.map_err(|_|Error::Network)?;
    let _: serde_json::Value=read_json(response).await?; Ok(())
}
async fn connect(url: String) -> Result<(Socket,Session),Error> {
    timeout(Duration::from_secs(15),async move {
        let config=WebSocketConfig::default().max_message_size(Some(262_144)).max_frame_size(Some(262_144));
        let (mut socket,_)=connect_async_with_config(url,Some(config),false).await.map_err(|_|Error::Network)?;
        loop {
            match socket.next().await.ok_or(Error::Network)?.map_err(|_|Error::Network)? {
                Message::Text(text) => {
                    let welcome: Envelope=serde_json::from_str(&text).map_err(|_|Error::Network)?;
                    if welcome.metadata.message_type!="session_welcome" { return Err(Error::Network); }
                    return Ok((socket,welcome.payload.session.ok_or(Error::Network)?));
                },
                Message::Ping(data) => socket.send(Message::Pong(data)).await.map_err(|_|Error::Network)?,
                _ => return Err(Error::Network),
            }
        }
    }).await.map_err(|_|Error::Network)?
}
fn keepalive(session: &Session) -> Duration { Duration::from_secs(session.keepalive_timeout_seconds.unwrap_or(30).clamp(10,600)) }
fn reconnect_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u|u.scheme()=="wss" && u.host_str()==Some("eventsub.wss.twitch.tv") && u.username().is_empty() && u.password().is_none() && u.port().is_none_or(|p|p==443) && u.fragment().is_none())
}
async fn session(app: Shared, config: &Config, token: &str, seen: &mut VecDeque<String>) -> Result<(),Error> {
    let account=validate_token(&app.client,&config.twitch_username,token).await?;
    let response=helix(&app.client,reqwest::Method::GET,"users",token,&account).query(&[("login",&config.twitch_channel)]).send().await.map_err(|_|Error::Network)?;
    let users: Data<User>=read_json(response).await?;
    let broadcaster=users.data.into_iter().next().ok_or(Error::Setup("Twitch channel not found. Check the channel login and save settings to retry."))?.id;
    let (mut socket,welcome)=connect(SOCKET.into()).await?;
    let mut keepalive=keepalive(&welcome); let mut deadline=Instant::now()+keepalive;
    let mut subscribing: Option<BoxFuture<'static,Result<(),Error>>>=Some(Box::pin(subscribe(app.client.clone(),account.clone(),token.into(),broadcaster.clone(),welcome.id)));
    let mut migrating: Option<BoxFuture<'static,Result<(Socket,Session),Error>>>=None;
    let mut pending: Option<BoxFuture<'static,Result<(),Error>>>=None;
    let mut validating: Option<BoxFuture<'static,Result<Account,Error>>>=None;
    let mut validation=tokio::time::interval_at(Instant::now()+Duration::from_secs(3600),Duration::from_secs(3600));
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return Err(Error::Network),
            result = async { subscribing.as_mut().unwrap().await }, if subscribing.is_some() => {
                subscribing=None; result?;
                *app.twitch_status.write().await=format!("EventSub connected · #{} · {}",config.twitch_channel,config.twitch_username);
            },
            result = async { migrating.as_mut().unwrap().await }, if migrating.is_some() => {
                migrating=None; let (replacement,welcome)=result?;
                // Subscriptions transfer automatically; receive on the old socket until this welcome.
                socket=replacement; keepalive=crate::twitch::keepalive(&welcome); deadline=Instant::now()+keepalive;
            },
            _ = validation.tick(), if validating.is_none() => {
                let client=app.client.clone(); let login=config.twitch_username.clone(); let token=token.to_owned();
                validating=Some(Box::pin(async move { validate_token(&client,&login,&token).await }));
            },
            result = async { validating.as_mut().unwrap().await }, if validating.is_some() => { validating=None; result?; },
            result = async { pending.as_mut().unwrap().await }, if pending.is_some() => { pending=None; result?; },
            frame = socket.next() => {
                match frame.ok_or(Error::Network)?.map_err(|_|Error::Network)? {
                    Message::Text(text) => {
                        let envelope: Envelope=serde_json::from_str(&text).map_err(|_|Error::Network)?;
                        match envelope.metadata.message_type.as_str() {
                            "session_keepalive" => { deadline=Instant::now()+keepalive; },
                            "session_reconnect" if migrating.is_none() => {
                                let url=envelope.payload.session.and_then(|s|s.reconnect_url).ok_or(Error::Network)?;
                                if !reconnect_url(&url) { return Err(Error::Network); } migrating=Some(Box::pin(connect(url)));
                            },
                            "revocation" => return Err(Error::Setup("Twitch revoked the chat subscription. Reauthorize the bot and save settings to reconnect.")),
                            "notification" => {
                                deadline=Instant::now()+keepalive;
                                if envelope.metadata.subscription_type.as_deref()!=Some("channel.chat.message") { continue; }
                                let event=envelope.payload.event.ok_or(Error::Network)?;
                                if !eligible(&event,&broadcaster,&account.user_id) || duplicate(seen,&event.message_id) || pending.is_some() { continue; }
                                let Some(prompt)=trigger(&event.message.text,&config.command,&config.twitch_username) else { continue; };
                                let input=ChatInput { platform:"twitch".into(),channel:broadcaster.clone(),user:event.chatter_user_login.clone(),message:prompt.into() };
                                let app=app.clone(); let token=token.to_owned(); let account=account.clone(); let broadcaster=broadcaster.clone();
                                pending=Some(Box::pin(async move {
                                    let result=server::respond(app.clone(),input,false).await;
                                    if let Ok(ChatOutput { reply:Some(reply),.. })=result {
                                        let message=format!("@{} {}",event.chatter_user_login,reply);
                                        let response=helix(&app.client,reqwest::Method::POST,"chat/messages",&token,&account)
                                            .json(&json!({"broadcaster_id":broadcaster,"sender_id":account.user_id,"message":message,"reply_parent_message_id":event.message_id})).send().await;
                                        let delivery=match response { Ok(r)=>read_json::<Data<Delivery>>(r).await,Err(_)=>Err(Error::Network) };
                                        match delivery {
                                            Ok(data) if data.data.first().is_some_and(|d|d.is_sent) => {},
                                            Err(Error::Auth) => return Err(Error::Auth),
                                            _ => { if let Ok(mut engine)=app.engine.try_lock() { engine.last_error=Some("Twitch did not confirm delivery. The message was not retried; check channel permissions, AutoMod, and rate limits.".into()); } }
                                        }
                                    } Ok(())
                                }));
                            }, _ => {},
                        }
                    },
                    Message::Ping(bytes) => { timeout(Duration::from_secs(5),socket.send(Message::Pong(bytes))).await.map_err(|_|Error::Network)?.map_err(|_|Error::Network)?; },
                    Message::Close(_) => return Err(Error::Network), _ => {},
                }
            }
        }
    }
}
fn eligible(event: &ChatEvent, broadcaster: &str, bot_id: &str) -> bool {
    event.broadcaster_user_id==broadcaster && event.chatter_user_id!=bot_id && valid_login(&event.chatter_user_login)
        && event.source_broadcaster_user_id.as_deref().is_none_or(|source|source==broadcaster)
        && !event.message_id.is_empty() && event.message_id.len()<=128
}
fn duplicate(seen: &mut VecDeque<String>, id: &str) -> bool {
    if seen.iter().any(|v|v==id) { return true; }
    if seen.len()==256 { seen.pop_front(); } seen.push_back(id.into()); false
}
fn trigger<'a>(text: &'a str, command: &str, login: &str) -> Option<&'a str> {
    let text=text.trim(); let first=text.split_whitespace().next()?;
    if !first.eq_ignore_ascii_case(command) && !first.trim_end_matches([',',':']).eq_ignore_ascii_case(&format!("@{login}")) { return None; }
    let message=text[first.len()..].trim(); if message.is_empty() { None } else { Some(message) }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn eventsub_json_and_subscription_contract() {
        let envelope: Envelope=serde_json::from_value(json!({"metadata":{"message_type":"notification","subscription_type":"channel.chat.message"},"payload":{"event":{"broadcaster_user_id":"1","chatter_user_id":"2","chatter_user_login":"viewer","message_id":"id","message":{"text":"!ai hi","fragments":[]},"source_broadcaster_user_id":null}}})).unwrap();
        let mut event=envelope.payload.event.unwrap(); assert!(eligible(&event,"1","3")); assert!(!eligible(&event,"1","2"));
        event.source_broadcaster_user_id=Some("other".into()); assert!(!eligible(&event,"1","3"));
        let body=subscription_body("1","2","session"); assert_eq!(body["transport"]["method"],"websocket"); assert_eq!(body["condition"]["user_id"],"2"); assert_eq!(body["type"],"channel.chat.message");
    }
    #[test] fn duplicate_window_is_bounded() {
        let mut seen=VecDeque::new(); assert!(!duplicate(&mut seen,"same")); assert!(duplicate(&mut seen,"same"));
        for i in 0..300 { duplicate(&mut seen,&i.to_string()); } assert_eq!(seen.len(),256);
    }
    #[test] fn reconnect_restricts_destination() {
        assert!(reconnect_url("wss://eventsub.wss.twitch.tv/ws?session=abc")); assert!(!reconnect_url("wss://evil.example/ws")); assert!(!reconnect_url("ws://eventsub.wss.twitch.tv/ws"));
    }
    #[test] fn trigger_requires_a_whole_command_or_bot_mention() {
        assert_eq!(trigger("!AI hello","!ai","bot"),Some("hello")); assert_eq!(trigger("@Bot, hi","!ai","bot"),Some("hi"));
        for text in ["!air hello","hello !ai","!ai","@botother hi"] { assert_eq!(trigger(text,"!ai","bot"),None); }
    }
}
