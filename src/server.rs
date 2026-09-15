use crate::{config::{self, Config}, engine::{ChatInput, ChatOutput, Engine}, secrets::{self, Secrets, Secret}};
use axum::{Router, Json, extract::{State, DefaultBodyLimit, Request}, http::{StatusCode, header}, middleware::{self, Next}, response::{Response, IntoResponse, Html}, routing::{get, post}};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, RwLock, watch};

pub struct App {
    pub config: RwLock<Config>, pub secrets: RwLock<Secrets>, pub engine: Mutex<Engine>,
    pub client: reqwest::Client, pub admin_token: String, pub origin: String, pub directory: PathBuf,
    pub changed: watch::Sender<u64>, pub twitch_status: RwLock<String>,
}
pub type Shared = Arc<App>;
type ApiResult = Result<Json<Value>, ApiError>;
pub struct ApiError(pub StatusCode, pub String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response { (self.0, Json(json!({"error":self.1}))).into_response() }
}
fn bad(message: String) -> ApiError { ApiError(StatusCode::BAD_REQUEST, message) }
fn busy() -> ApiError { ApiError(StatusCode::CONFLICT, "An AI request or settings update is running. Try again shortly.".into()) }

pub fn router(app: Shared) -> Router {
    let api = Router::new()
        .route("/api/config", get(get_config).put(save_config))
        .route("/api/keys", post(save_key))
        .route("/api/status", get(status))
        .route("/api/preview", post(preview))
        .route("/api/memory/clear", post(clear_memory))
        .route_layer(middleware::from_fn_with_state(app.clone(), authorize));
    Router::new().merge(api)
        .route("/", get(|| async { Html(include_str!("../ui/index.html")) }))
        .route("/app.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../ui/app.js")) }))
        .route("/style.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("../ui/style.css")) }))
        .layer(DefaultBodyLimit::max(32_768))
        .layer(middleware::from_fn_with_state(app.clone(), local_only))
        .with_state(app)
}

async fn local_only(State(app): State<Shared>, request: Request, next: Next) -> Response {
    let expected_host = app.origin.trim_start_matches("http://");
    if request.headers().get(header::HOST).and_then(|h|h.to_str().ok()) != Some(expected_host)
        || request.headers().get(header::ORIGIN).is_some_and(|h| h.to_str().ok() != Some(app.origin.as_str())) {
        return ApiError(StatusCode::FORBIDDEN, "Only the local configuration page can access this service.".into()).into_response();
    }
    let mut response = next.run(request).await;
    for (name, value) in [
        ("cache-control", "no-store"), ("x-content-type-options", "nosniff"), ("referrer-policy", "no-referrer"),
        ("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
    ] { response.headers_mut().insert(header::HeaderName::from_static(name), header::HeaderValue::from_static(value)); }
    response
}
async fn authorize(State(app): State<Shared>, request: Request, next: Next) -> Response {
    let token = request.headers().get(header::AUTHORIZATION).and_then(|v|v.to_str().ok()).and_then(|v|v.strip_prefix("Bearer ")).unwrap_or("");
    if !constant_time_equal(token.as_bytes(), app.admin_token.as_bytes()) {
        return ApiError(StatusCode::UNAUTHORIZED, "Open the configuration link printed by the running bot.".into()).into_response();
    }
    next.run(request).await
}
fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b).fold(0u8, |acc, (x,y)| acc | (x ^ y)) == 0
}
pub fn notify(app: &App) { app.changed.send_modify(|v| *v = v.wrapping_add(1)); }

async fn get_config(State(app): State<Shared>) -> Json<Value> {
    let config = app.config.read().await;
    let secrets = app.secrets.read().await;
    Json(json!({"config":*config, "keys":secrets.status(), "warning":secrets.warning}))
}
async fn save_config(State(app): State<Shared>, Json(config): Json<Config>) -> ApiResult {
    config.validate().map_err(bad)?;
    let mut engine = app.engine.try_lock().map_err(|_|busy())?;
    let dir = app.directory.clone();
    let bytes = serde_json::to_vec_pretty(&config).map_err(|_|bad("Could not serialize settings.".into()))?;
    tokio::task::spawn_blocking(move || config::atomic_write(&dir, "config.json", &bytes)).await
        .map_err(|_|bad("Could not save settings.".into()))?.map_err(|_|bad("Could not write settings. Check directory permissions.".into()))?;
    *app.config.write().await = config;
    engine.clear();
    notify(&app);
    Ok(Json(json!({"saved":true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyUpdate { id: String, key: String, remember: bool, #[serde(default)] remove: bool }
async fn save_key(State(app): State<Shared>, Json(mut update): Json<KeyUpdate>) -> ApiResult {
    if !secrets::IDS.contains(&update.id.as_str()) { return Err(bad("Unknown credential type.".into())); }
    update.key = update.key.trim().strip_prefix("oauth:").unwrap_or(update.key.trim()).to_string();
    if !update.remove && (update.key.is_empty() || update.key.len() > 4096 || update.key.chars().any(char::is_control)) { return Err(bad("Enter a valid credential.".into())); }
    let _gate = app.engine.try_lock().map_err(|_|busy())?;
    if update.remember || update.remove {
        let id = update.id.clone(); let value = update.key.clone(); let remove = update.remove;
        tokio::task::spawn_blocking(move || if remove { secrets::forget(&id) } else { secrets::persist(&id, &value) }).await
            .map_err(|_|bad("Credential store task failed.".into()))?.map_err(bad)?;
    }
    let mut keys = app.secrets.write().await;
    if update.remove { keys.values.remove(&update.id); }
    else { keys.values.insert(update.id, Secret { value:update.key, source: if update.remember { "keychain" } else { "session" } }); }
    notify(&app);
    Ok(Json(json!({"saved":true, "keys":keys.status()})))
}
async fn status(State(app): State<Shared>) -> Json<Value> {
    let twitch = app.twitch_status.read().await.clone();
    if let Ok(engine) = app.engine.try_lock() {
        Json(json!({"busy":false,"conversations":engine.count(),"replies":engine.replies,"last_error":engine.last_error,"twitch":twitch}))
    } else { Json(json!({"busy":true,"twitch":twitch})) }
}
pub async fn respond(app: Shared, input: ChatInput, preview: bool) -> Result<ChatOutput, String> {
    let Ok(mut engine) = app.engine.try_lock() else { return Ok(ChatOutput::skip("busy")); };
    let config = app.config.read().await.clone();
    let key = app.secrets.read().await.get(config.provider.id());
    engine.chat(&app.client, &config, &key, input, preview).await
}
async fn preview(State(app): State<Shared>, Json(input): Json<ChatInput>) -> ApiResult {
    let output = respond(app, input, true).await.map_err(bad)?;
    Ok(Json(json!(output)))
}
async fn clear_memory(State(app): State<Shared>) -> ApiResult {
    app.engine.try_lock().map_err(|_|busy())?.clear();
    Ok(Json(json!({"cleared":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::{Body, to_bytes}, http::Request};
    use tower::ServiceExt;
    pub fn test_app(dir: PathBuf) -> Shared {
        Arc::new(App { config:RwLock::new(Config::default()), secrets:RwLock::new(Secrets { values:Default::default(), warning:None }),
            engine:Mutex::new(Engine::default()), client:crate::provider::client().unwrap(), admin_token:"test-token".into(),
            origin:"http://127.0.0.1:9847".into(), directory:dir, changed:watch::channel(0).0, twitch_status:RwLock::new("Disabled".into()) })
    }
    #[tokio::test] async fn rejects_unauthenticated_and_foreign_origins() {
        let dir = tempfile::tempdir().unwrap(); let app = router(test_app(dir.path().into()));
        for (host, origin, token, expected) in [
            ("127.0.0.1:9847", None, None, StatusCode::UNAUTHORIZED),
            ("evil.example", None, Some("test-token"), StatusCode::FORBIDDEN),
            ("127.0.0.1:9847", Some("https://evil.example"), Some("test-token"), StatusCode::FORBIDDEN),
            ("127.0.0.1:9847", Some("http://127.0.0.1:9847"), Some("test-token"), StatusCode::OK),
        ] {
            let mut req = Request::builder().uri("/api/config").header("host",host);
            if let Some(o)=origin { req=req.header("origin",o); } if let Some(t)=token { req=req.header("authorization",format!("Bearer {t}")); }
            assert_eq!(app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap().status(), expected);
        }
    }
    #[tokio::test] async fn secrets_are_write_only_and_preview_does_not_send_without_setup() {
        let dir = tempfile::tempdir().unwrap(); let state = test_app(dir.path().into()); let app = router(state);
        let req = |path: &str, body: Value| Request::builder().method("POST").uri(path).header("host","127.0.0.1:9847").header("authorization","Bearer test-token").header("content-type","application/json").body(Body::from(body.to_string())).unwrap();
        let r=app.clone().oneshot(req("/api/keys",json!({"id":"openai","key":"secret-value","remember":false}))).await.unwrap();
        assert_eq!(r.status(),StatusCode::OK);
        let bytes=to_bytes(r.into_body(),32768).await.unwrap(); assert!(!String::from_utf8_lossy(&bytes).contains("secret-value"));
        let r=app.oneshot(req("/api/preview",json!({"platform":"preview","channel":"preview","user":"you","message":"Hello"}))).await.unwrap();
        assert_eq!(r.status(),StatusCode::BAD_REQUEST);
    }
}
