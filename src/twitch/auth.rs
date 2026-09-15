//! Public-client Twitch device OAuth. Tokens never enter UI status or diagnostics.
use super::auth_store::{CredentialBundle, CredentialStore, FileCredentialStore};
use serde::Deserialize;
use std::{
    fmt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, watch};

const SCOPES: &str = "user:read:chat user:write:chat";
const STORE_WARNING: &str = "Twitch is connected for this session only. Its credential file could not be saved; check the app data folder permissions and reconnect after restarting.";

#[derive(Clone, Debug, Default)]
pub struct AuthSnapshot {
    pub connected: bool,
    pub login: Option<String>,
    pub persistent: bool,
    pub warning: Option<String>,
}

#[derive(Clone)]
pub struct AccessSession {
    pub access_token: String,
    pub client_id: String,
    pub login: String,
    pub user_id: String,
    pub scopes: Vec<String>,
    pub generation: u64,
}

impl fmt::Debug for AccessSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessSession")
            .field("login", &self.login)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

pub struct DeviceGrant {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    client_id: String,
    device_code: String,
    interval: u64,
    deadline: tokio::time::Instant,
    epoch: u64,
}

impl fmt::Debug for DeviceGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceGrant")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    MissingClientId,
    Cancelled,
    AuthorizationExpired,
    Denied,
    Reauthorize,
    Network,
    Storage,
    Protocol,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingClientId => "A public Twitch application Client ID is required before connecting.",
            Self::Cancelled => "Twitch authorization was cancelled.",
            Self::AuthorizationExpired => "The Twitch authorization code expired. Connect Twitch to try again.",
            Self::Denied => "Twitch authorization was declined.",
            Self::Reauthorize => "Twitch authorization is no longer valid. Reconnect Twitch.",
            Self::Network => "Twitch could not be reached. Check your connection and retry.",
            Self::Storage => "Could not remove the saved Twitch credential file. Check the app data folder permissions and retry.",
            Self::Protocol => "Twitch returned an unexpected authorization response. Try connecting again.",
        })
    }
}
impl std::error::Error for AuthError {}

#[derive(Default)]
struct State {
    bundle: Option<CredentialBundle>,
    generation: u64,
    persistent: bool,
    validated: bool,
    warning: Option<String>,
    // Remember failed deletions so Disconnect can be retried without a live token.
    stored_client_id: Option<String>,
}

#[derive(Clone)]
pub struct AuthManager {
    client: reqwest::Client,
    state: Arc<Mutex<State>>,
    refresh_gate: Arc<Mutex<()>>,
    storage_gate: Arc<Mutex<()>>,
    epoch: watch::Sender<u64>,
    store: Arc<dyn CredentialStore>,
    base: String,
    workers: Arc<std::sync::atomic::AtomicUsize>,
    settled: Arc<tokio::sync::Notify>,
}

struct WorkerCompletion {
    workers: Arc<std::sync::atomic::AtomicUsize>,
    settled: Arc<tokio::sync::Notify>,
}
impl Drop for WorkerCompletion {
    fn drop(&mut self) {
        self.workers
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.settled.notify_one();
    }
}

#[derive(Clone, Copy)]
enum AdoptionGuard {
    Device(u64),
    Refresh(u64),
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}
#[derive(Deserialize)]
struct Identity {
    client_id: String,
    user_id: String,
    login: String,
    scopes: Vec<String>,
}
#[derive(Deserialize, Default)]
struct ErrorResponse {
    #[serde(default)]
    message: String,
    #[serde(default)]
    error: String,
}

impl AuthManager {
    pub fn new(client: reqwest::Client, directory: PathBuf) -> Self {
        Self::with_store(
            client,
            Arc::new(FileCredentialStore::new(directory)),
            "https://id.twitch.tv/oauth2".into(),
        )
    }

    fn with_store(client: reqwest::Client, store: Arc<dyn CredentialStore>, base: String) -> Self {
        let (epoch, _) = watch::channel(0);
        Self {
            client,
            state: Arc::new(Mutex::new(State::default())),
            refresh_gate: Arc::new(Mutex::new(())),
            storage_gate: Arc::new(Mutex::new(())),
            epoch,
            store,
            base,
            workers: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            settled: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn worker(&self) -> WorkerCompletion {
        self.workers
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        WorkerCompletion {
            workers: self.workers.clone(),
            settled: self.settled.clone(),
        }
    }

    /// On normal shutdown, await this before dropping the runtime. An external timeout
    /// may still be needed for a stalled credential-file write; abrupt termination cannot guarantee
    /// recovery of a single-use refresh token.
    pub async fn finish_pending(&self) {
        loop {
            let notified = self.settled.notified();
            if self.workers.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    fn next_epoch(&self) -> u64 {
        self.epoch.send_modify(|v| *v = v.wrapping_add(1));
        *self.epoch.borrow()
    }

    pub fn cancel_authorization(&self) {
        self.next_epoch();
    }

    fn check_epoch(&self, epoch: u64) -> Result<(), AuthError> {
        if *self.epoch.borrow() == epoch {
            Ok(())
        } else {
            Err(AuthError::Cancelled)
        }
    }

    async fn cancelled(&self, epoch: u64) {
        let mut receiver = self.epoch.subscribe();
        let _ = receiver.wait_for(|v| *v != epoch).await;
    }

    pub async fn snapshot(&self) -> AuthSnapshot {
        let state = self.state.lock().await;
        AuthSnapshot {
            connected: state.bundle.is_some() && state.validated,
            login: state.bundle.as_ref().map(|b| b.login.clone()),
            persistent: state.persistent,
            warning: state.warning.clone(),
        }
    }

    pub async fn current(&self) -> Option<AccessSession> {
        self.access(false).await
    }

    /// Allows the independent validation task to retry a stored login after offline startup.
    pub async fn has_credentials(&self) -> bool {
        self.state.lock().await.bundle.is_some()
    }

    async fn access(&self, allow_unvalidated: bool) -> Option<AccessSession> {
        let state = self.state.lock().await;
        if !state.validated && !allow_unvalidated {
            return None;
        }
        state.bundle.as_ref().map(|b| AccessSession {
            access_token: b.access_token.clone(),
            client_id: b.client_id.clone(),
            login: b.login.clone(),
            user_id: b.user_id.clone(),
            scopes: b.scopes.clone(),
            generation: state.generation,
        })
    }

    /// Loads only this app's OAuth entry; never reads the legacy pasted token entry.
    pub async fn restore(&self, client_id: &str) -> Result<AuthSnapshot, AuthError> {
        let client_id = checked_client_id(client_id)?;
        let epoch = self.next_epoch();
        let store = self.store.clone();
        let id = client_id.to_owned();
        let result = {
            let _gate = self.storage_gate.lock().await;
            tokio::task::spawn_blocking(move || store.load(&id))
                .await
                .map_err(|_| AuthError::Protocol)?
        };
        self.check_epoch(epoch)?;
        match result {
            Ok(Some(bundle)) => {
                if bundle.version != 1
                    || bundle.client_id != client_id
                    || bundle.access_token.is_empty()
                    || bundle.refresh_token.is_empty()
                {
                    return Err(AuthError::Reauthorize);
                }
                let mut state = self.state.lock().await;
                self.check_epoch(epoch)?;
                state.stored_client_id = Some(client_id.into());
                state.bundle = Some(bundle);
                state.validated = false;
                state.generation = state.generation.wrapping_add(1);
                state.persistent = true;
                state.warning = None;
                drop(state);
                self.validate().await
            }
            Ok(None) => Ok(self.snapshot().await),
            Err(()) => {
                self.state.lock().await.warning = Some("Saved Twitch login could not be read from its credential file. Check the app data folder permissions or connect again.".into());
                Ok(self.snapshot().await)
            }
        }
    }

    pub async fn begin_device(&self, client_id: &str) -> Result<DeviceGrant, AuthError> {
        let client_id = checked_client_id(client_id)?;
        let epoch = self.next_epoch();
        let request = self
            .client
            .post(format!("{}/device", self.base))
            .form(&[("client_id", client_id), ("scopes", SCOPES)]);
        let (status, body) = tokio::select! {
            _ = self.cancelled(epoch) => return Err(AuthError::Cancelled),
            result = request_json(request) => result?,
        };
        if !status.is_success() {
            return Err(classify_error(status, &body));
        }
        let response: DeviceResponse =
            serde_json::from_value(body).map_err(|_| AuthError::Protocol)?;
        if response.device_code.is_empty()
            || response.user_code.is_empty()
            || response.expires_in == 0
            || response.expires_in > 86400
            || !valid_verification_uri(&response.verification_uri)
        {
            return Err(AuthError::Protocol);
        }
        self.check_epoch(epoch)?;
        Ok(DeviceGrant {
            user_code: response.user_code,
            verification_uri: response.verification_uri,
            expires_in: response.expires_in,
            device_code: response.device_code,
            client_id: client_id.into(),
            interval: response.interval.max(1).min(response.expires_in),
            deadline: tokio::time::Instant::now() + Duration::from_secs(response.expires_in),
            epoch,
        })
    }

    pub async fn poll_device(&self, grant: DeviceGrant) -> Result<AuthSnapshot, AuthError> {
        let mut interval = grant.interval;
        loop {
            tokio::select! {
                _ = self.cancelled(grant.epoch) => return Err(AuthError::Cancelled),
                _ = tokio::time::sleep_until(grant.deadline) => return Err(AuthError::AuthorizationExpired),
                _ = tokio::time::sleep(Duration::from_secs(interval)) => {},
            }
            let request = self.client.post(format!("{}/token", self.base)).form(&[
                ("client_id", grant.client_id.as_str()),
                ("device_code", grant.device_code.as_str()),
                ("scopes", SCOPES),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ]);
            let response = tokio::select! {
                _ = self.cancelled(grant.epoch) => return Err(AuthError::Cancelled),
                _ = tokio::time::sleep_until(grant.deadline) => return Err(AuthError::AuthorizationExpired),
                result = request_json(request) => result,
            };
            let (status, body) = match response {
                Ok(value) => value,
                Err(AuthError::Network) => {
                    interval = interval.saturating_add(5).min(60.max(interval));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if status.is_success() {
                let token: TokenResponse =
                    serde_json::from_value(body).map_err(|_| AuthError::Protocol)?;
                let identity = self.identity(&grant.client_id, &token.access_token).await?;
                self.adopt(bundle(&grant.client_id, token, identity)?, grant.epoch)
                    .await?;
                return Ok(self.snapshot().await);
            }
            let error = error_code(&body);
            match error.as_str() {
                "authorization_pending" => {}
                "slow_down" => {
                    interval = interval.saturating_add(5);
                }
                "access_denied" | "authorization_declined" => return Err(AuthError::Denied),
                "expired_token" | "invalid device code" => {
                    return Err(AuthError::AuthorizationExpired);
                }
                _ if status.is_server_error() || status.as_u16() == 429 => {
                    interval = interval.saturating_add(5).min(60.max(interval));
                }
                _ => return Err(classify_error(status, &body)),
            }
        }
    }

    async fn identity(&self, client_id: &str, token: &str) -> Result<Identity, AuthError> {
        let (status, body) = request_json(
            self.client
                .get(format!("{}/validate", self.base))
                .header("Authorization", format!("OAuth {token}")),
        )
        .await?;
        if !status.is_success() {
            return Err(classify_error(status, &body));
        }
        let identity: Identity = serde_json::from_value(body).map_err(|_| AuthError::Protocol)?;
        if identity.client_id != client_id
            || identity.user_id.is_empty()
            || identity.login.is_empty()
            || !SCOPES
                .split_whitespace()
                .all(|scope| identity.scopes.iter().any(|s| s == scope))
        {
            return Err(AuthError::Reauthorize);
        }
        Ok(identity)
    }

    /// Call at startup and hourly. A definite invalid-token response triggers refresh.
    pub async fn validate(&self) -> Result<AuthSnapshot, AuthError> {
        let current = self.access(true).await.ok_or(AuthError::Reauthorize)?;
        match self
            .identity(&current.client_id, &current.access_token)
            .await
        {
            Ok(identity) if identity.user_id == current.user_id => {
                let mut state = self.state.lock().await;
                if state.generation != current.generation {
                    return Err(AuthError::Cancelled);
                }
                state.validated = true;
                drop(state);
                Ok(self.snapshot().await)
            }
            Ok(_) => {
                self.invalidate(current.generation).await;
                Err(AuthError::Reauthorize)
            }
            Err(AuthError::Reauthorize) => {
                self.refresh_after_401(current.generation).await?;
                Ok(self.snapshot().await)
            }
            Err(error) => Err(error),
        }
    }

    /// Only a proven 401 should call this. Concurrent callers share the new generation.
    pub async fn refresh_after_401(
        &self,
        used_generation: u64,
    ) -> Result<AccessSession, AuthError> {
        // Dropping a transport future must not abandon a single-use refresh after Twitch
        // has consumed it. The owned task finishes rotation/persistence independently.
        let manager = self.clone();
        let completion = self.worker();
        tokio::spawn(async move {
            let _completion = completion;
            manager.refresh_inner(used_generation).await
        })
        .await
        .map_err(|_| AuthError::Protocol)?
    }

    async fn refresh_inner(&self, used_generation: u64) -> Result<AccessSession, AuthError> {
        let _refresh = self.refresh_gate.lock().await;
        let current = self.access(true).await.ok_or(AuthError::Reauthorize)?;
        if current.generation != used_generation {
            return Ok(current);
        }
        let prior = {
            let state = self.state.lock().await;
            // A new device login may have completed between the access snapshot and
            // this lock. Never exchange that account's refresh token for an older request.
            if state.generation != used_generation {
                return Err(AuthError::Cancelled);
            }
            state.bundle.clone().ok_or(AuthError::Reauthorize)?
        };
        let request = self.client.post(format!("{}/token", self.base)).form(&[
            ("client_id", prior.client_id.as_str()),
            ("refresh_token", prior.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ]);
        // Once dispatched, finish processing a successful rotation even if cancellation
        // arrives; adopt's generation check prevents it resurrecting a disconnected login.
        let (status, body) = request_json(request).await?;
        if !status.is_success() {
            let error = classify_error(status, &body);
            if error == AuthError::Reauthorize {
                self.invalidate(used_generation).await;
            }
            return Err(error);
        }
        let token: TokenResponse = serde_json::from_value(body).map_err(|_| AuthError::Protocol)?;
        // Persist the single-use rotation immediately. The original account/scopes are
        // retained, then checked against /validate; there is no recoverable old-token slot.
        let identity = Identity {
            client_id: prior.client_id.clone(),
            user_id: prior.user_id.clone(),
            login: prior.login.clone(),
            scopes: prior.scopes.clone(),
        };
        let next = bundle(&prior.client_id, token, identity)?;
        self.adopt_inner(next, AdoptionGuard::Refresh(used_generation))
            .await?;
        let updated = self.current().await.ok_or(AuthError::Reauthorize)?;
        match self
            .identity(&updated.client_id, &updated.access_token)
            .await
        {
            Ok(identity) if identity.user_id == updated.user_id => Ok(updated),
            Ok(_) | Err(AuthError::Reauthorize) => {
                self.invalidate(updated.generation).await;
                Err(AuthError::Reauthorize)
            }
            Err(error) => Err(error),
        }
    }

    async fn adopt(&self, bundle: CredentialBundle, epoch: u64) -> Result<(), AuthError> {
        let manager = self.clone();
        let completion = self.worker();
        tokio::spawn(async move {
            let _completion = completion;
            manager
                .adopt_inner(bundle, AdoptionGuard::Device(epoch))
                .await
        })
        .await
        .map_err(|_| AuthError::Protocol)?
    }

    fn adoption_current(&self, guard: AdoptionGuard, state: &State) -> bool {
        match guard {
            AdoptionGuard::Device(epoch) => self.check_epoch(epoch).is_ok(),
            AdoptionGuard::Refresh(generation) => {
                state.generation == generation && state.bundle.is_some()
            }
        }
    }

    async fn adopt_inner(
        &self,
        bundle: CredentialBundle,
        guard: AdoptionGuard,
    ) -> Result<(), AuthError> {
        let _gate = self.storage_gate.lock().await;
        if !self.adoption_current(guard, &*self.state.lock().await) {
            return Err(AuthError::Cancelled);
        }
        // Record the destination before blocking storage. A concurrent logout advances
        // the epoch then waits on storage_gate and must delete even a stale completed save.
        self.state.lock().await.stored_client_id = Some(bundle.client_id.clone());
        let store = self.store.clone();
        let saved = bundle.clone();
        let persistent = tokio::task::spawn_blocking(move || store.save(&saved))
            .await
            .is_ok_and(|result| result.is_ok());
        let mut state = self.state.lock().await;
        if !self.adoption_current(guard, &state) {
            drop(state);
            // Cancellation can arrive while the credential file is being written. Remove that stale
            // grant rather than leaving a cancelled login available on the next launch.
            let store = self.store.clone();
            let client_id = bundle.client_id.clone();
            let removed = tokio::task::spawn_blocking(move || store.delete(&client_id))
                .await
                .is_ok_and(|result| result.is_ok());
            let mut state = self.state.lock().await;
            state.persistent = false;
            state.warning = if !removed {
                Some(AuthError::Storage.to_string())
            } else if state.bundle.is_some() {
                Some(STORE_WARNING.into())
            } else {
                None
            };
            return Err(AuthError::Cancelled);
        }
        state.stored_client_id = Some(bundle.client_id.clone());
        state.bundle = Some(bundle);
        state.validated = true;
        state.generation = state.generation.wrapping_add(1);
        state.persistent = persistent;
        state.warning = if persistent {
            None
        } else {
            Some(STORE_WARNING.into())
        };
        Ok(())
    }

    async fn invalidate(&self, generation: u64) {
        let mut state = self.state.lock().await;
        if state.generation == generation {
            state.bundle = None;
            state.validated = false;
            state.persistent = false;
            state.warning = Some(AuthError::Reauthorize.to_string());
        }
    }

    pub async fn logout(&self) -> Result<(), AuthError> {
        self.next_epoch();
        let (prior, client_id) = {
            let mut state = self.state.lock().await;
            let prior = state.bundle.take();
            state.validated = false;
            state.generation = state.generation.wrapping_add(1);
            state.persistent = false;
            state.warning = None;
            (prior, state.stored_client_id.clone())
        };
        let _gate = self.storage_gate.lock().await;
        let mut deletion_failed = false;
        if let Some(client_id) = client_id {
            let store = self.store.clone();
            deletion_failed = !tokio::task::spawn_blocking(move || store.delete(&client_id))
                .await
                .is_ok_and(|result| result.is_ok());
            if !deletion_failed {
                self.state.lock().await.stored_client_id = None;
            }
        }
        // Forget local credentials first. Revocation is best-effort and bounded.
        if let Some(prior) = prior {
            let _ = self
                .client
                .post(format!("{}/revoke", self.base))
                .timeout(Duration::from_secs(5))
                .form(&[
                    ("client_id", prior.client_id),
                    ("token", prior.access_token),
                ])
                .send()
                .await;
        }
        if deletion_failed {
            self.state.lock().await.warning = Some(AuthError::Storage.to_string());
            Err(AuthError::Storage)
        } else {
            Ok(())
        }
    }
}

fn checked_client_id(client_id: &str) -> Result<&str, AuthError> {
    let value = client_id.trim();
    if value.is_empty() {
        Err(AuthError::MissingClientId)
    } else if value.len() > 128 || !value.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Err(AuthError::Protocol)
    } else {
        Ok(value)
    }
}

fn valid_verification_uri(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("www.twitch.tv")
            && url.path() == "/activate"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none_or(|port| port == 443)
            && url.fragment().is_none()
    })
}

fn bundle(
    client_id: &str,
    token: TokenResponse,
    identity: Identity,
) -> Result<CredentialBundle, AuthError> {
    if token.access_token.is_empty() || token.refresh_token.is_empty() || token.expires_in == 0 {
        return Err(AuthError::Protocol);
    }
    Ok(CredentialBundle {
        version: 1,
        client_id: client_id.into(),
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(token.expires_in),
        user_id: identity.user_id,
        login: identity.login,
        scopes: identity.scopes,
    })
}

fn error_code(body: &serde_json::Value) -> String {
    let response: ErrorResponse = serde_json::from_value(body.clone()).unwrap_or_default();
    if response.error.is_empty() {
        response.message.to_ascii_lowercase()
    } else {
        response.error.to_ascii_lowercase()
    }
}

fn classify_error(status: reqwest::StatusCode, body: &serde_json::Value) -> AuthError {
    if status.is_server_error() || status.as_u16() == 429 {
        return AuthError::Network;
    }
    match error_code(body).as_str() {
        "access_denied" | "authorization_declined" => AuthError::Denied,
        "expired_token" | "invalid device code" => AuthError::AuthorizationExpired,
        "invalid refresh token" | "invalid_grant" => AuthError::Reauthorize,
        _ if status.as_u16() == 401 || status.as_u16() == 403 => AuthError::Reauthorize,
        _ => AuthError::Protocol,
    }
}

async fn request_json(
    request: reqwest::RequestBuilder,
) -> Result<(reqwest::StatusCode, serde_json::Value), AuthError> {
    let mut response = request
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| AuthError::Network)?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Network)? {
        if bytes.len() + chunk.len() > 65536 {
            return Err(AuthError::Protocol);
        }
        bytes.extend_from_slice(&chunk);
    }
    // Upstream text never escapes as a UI error or diagnostic.
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State as HttpState,
        http::StatusCode,
        routing::{get, post},
    };
    use serde_json::json;
    use std::sync::{
        Mutex as SyncMutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct MemoryStore {
        value: SyncMutex<Option<CredentialBundle>>,
        fail: bool,
        write_started: Option<Arc<tokio::sync::Notify>>,
        write_release: Option<Arc<(SyncMutex<bool>, std::sync::Condvar)>>,
    }
    impl CredentialStore for MemoryStore {
        fn load(&self, _: &str) -> Result<Option<CredentialBundle>, ()> {
            Ok(self.value.lock().unwrap().clone())
        }
        fn save(&self, bundle: &CredentialBundle) -> Result<(), ()> {
            if self.fail {
                return Err(());
            }
            *self.value.lock().unwrap() = Some(bundle.clone());
            if let Some(started) = &self.write_started {
                started.notify_one();
            }
            if let Some(release) = &self.write_release {
                let (lock, condition) = &**release;
                let _guard = condition
                    .wait_while(lock.lock().unwrap(), |ready| !*ready)
                    .unwrap();
            }
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<(), ()> {
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }
    fn credential() -> CredentialBundle {
        CredentialBundle {
            version: 1,
            client_id: "client".into(),
            access_token: "old-access".into(),
            refresh_token: "old-refresh".into(),
            expires_at: 0,
            user_id: "123".into(),
            login: "bot".into(),
            scopes: SCOPES.split_whitespace().map(str::to_owned).collect(),
        }
    }
    async fn fixture(
        store: Arc<MemoryStore>,
    ) -> (AuthManager, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let router = Router::new()
            .route("/device", post(|| async { Json(json!({"device_code":"device-secret","user_code":"VISIBLE","verification_uri":"https://www.twitch.tv/activate?device-code=VISIBLE","expires_in":60,"interval":1})) }))
            .route("/validate", get(|| async { Json(json!({"client_id":"client","user_id":"123","login":"bot","scopes":["user:read:chat","user:write:chat"]})) }))
            .route("/token", post(|HttpState(calls): HttpState<Arc<AtomicUsize>>| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Json(json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":14400}))
            }))
            .route("/revoke", post(|| async { StatusCode::OK }))
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let manager =
            AuthManager::with_store(reqwest::Client::new(), store, format!("http://{address}"));
        (manager, calls, task)
    }

    #[tokio::test]
    async fn concurrent_401s_rotate_once_and_persist_new_pair() {
        let store = Arc::new(MemoryStore::default());
        *store.value.lock().unwrap() = Some(credential());
        let (manager, calls, task) = fixture(store.clone()).await;
        manager.restore("client").await.unwrap();
        let generation = manager.current().await.unwrap().generation;
        let (first, second) = tokio::join!(
            manager.refresh_after_401(generation),
            manager.refresh_after_401(generation)
        );
        assert_eq!(first.unwrap().generation, second.unwrap().generation);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.value.lock().unwrap().as_ref().unwrap().refresh_token,
            "new-refresh"
        );
        manager.logout().await.unwrap();
        assert!(manager.current().await.is_none());
        assert!(store.value.lock().unwrap().is_none());
        task.abort();
    }

    #[tokio::test]
    async fn stale_adoption_cannot_restore_disconnected_account() {
        let store = Arc::new(MemoryStore::default());
        let (manager, _, task) = fixture(store.clone()).await;
        let epoch = *manager.epoch.borrow();
        manager.cancel_authorization();
        assert_eq!(
            manager.adopt(credential(), epoch).await.unwrap_err(),
            AuthError::Cancelled
        );
        assert!(store.value.lock().unwrap().is_none());
        assert!(manager.current().await.is_none());
        task.abort();
    }

    #[tokio::test]
    async fn failed_storage_keeps_explicit_session_only_connection() {
        let store = Arc::new(MemoryStore {
            fail: true,
            ..MemoryStore::default()
        });
        let (manager, _, task) = fixture(store).await;
        manager.adopt(credential(), 0).await.unwrap();
        let snapshot = manager.snapshot().await;
        assert!(snapshot.connected && !snapshot.persistent);
        assert!(snapshot.warning.is_some());
        task.abort();
    }

    #[tokio::test]
    async fn missing_client_id_needs_no_network_or_credentials() {
        let (manager, _, task) = fixture(Arc::new(MemoryStore::default())).await;
        assert_eq!(
            manager.begin_device(" ").await.unwrap_err(),
            AuthError::MissingClientId
        );
        task.abort();
    }

    #[tokio::test]
    async fn device_login_validates_account_and_stores_bundle() {
        let store = Arc::new(MemoryStore::default());
        let (manager, calls, task) = fixture(store.clone()).await;
        let grant = manager.begin_device("client").await.unwrap();
        assert_eq!(grant.user_code, "VISIBLE");
        assert!(!format!("{grant:?}").contains("device-secret"));
        let snapshot = manager.poll_device(grant).await.unwrap();
        assert_eq!(snapshot.login.as_deref(), Some("bot"));
        assert!(snapshot.connected && snapshot.persistent);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.value.lock().unwrap().as_ref().unwrap().user_id, "123");
        task.abort();
    }

    #[tokio::test]
    async fn cancel_and_expiry_stop_polling_without_exchanging_tokens() {
        let (manager, calls, task) = fixture(Arc::new(MemoryStore::default())).await;
        let grant = manager.begin_device("client").await.unwrap();
        manager.cancel_authorization();
        assert_eq!(
            manager.poll_device(grant).await.unwrap_err(),
            AuthError::Cancelled
        );
        let mut grant = manager.begin_device("client").await.unwrap();
        grant.deadline = tokio::time::Instant::now();
        assert_eq!(
            manager.poll_device(grant).await.unwrap_err(),
            AuthError::AuthorizationExpired
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        task.abort();
    }

    #[tokio::test]
    async fn login_with_wrong_client_id_is_never_persisted() {
        let store = Arc::new(MemoryStore::default());
        let (manager, _, task) = fixture(store.clone()).await;
        let mut grant = manager.begin_device("otherclient").await.unwrap();
        grant.interval = 0;
        assert_eq!(
            manager.poll_device(grant).await.unwrap_err(),
            AuthError::Reauthorize
        );
        assert!(manager.current().await.is_none());
        assert!(store.value.lock().unwrap().is_none());
        task.abort();
    }

    #[tokio::test]
    async fn cancelling_while_credential_file_writes_removes_stale_login() {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new((SyncMutex::new(false), std::sync::Condvar::new()));
        let store = Arc::new(MemoryStore {
            write_started: Some(started.clone()),
            write_release: Some(release.clone()),
            ..MemoryStore::default()
        });
        let (manager, _, server) = fixture(store.clone()).await;
        let manager = Arc::new(manager);
        let worker_manager = manager.clone();
        let worker = tokio::spawn(async move { worker_manager.adopt(credential(), 0).await });
        started.notified().await;
        manager.cancel_authorization();
        *release.0.lock().unwrap() = true;
        release.1.notify_one();
        assert_eq!(worker.await.unwrap().unwrap_err(), AuthError::Cancelled);
        assert!(store.value.lock().unwrap().is_none());
        assert!(manager.current().await.is_none());
        server.abort();
    }

    #[tokio::test]
    async fn revoked_refresh_requires_reauthorization() {
        let router = Router::new().route(
            "/token",
            post(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"message":"Invalid refresh token"})),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let manager = AuthManager::with_store(
            reqwest::Client::new(),
            Arc::new(MemoryStore::default()),
            format!("http://{address}"),
        );
        manager.adopt(credential(), 0).await.unwrap();
        let generation = manager.current().await.unwrap().generation;
        assert_eq!(
            manager.refresh_after_401(generation).await.unwrap_err(),
            AuthError::Reauthorize
        );
        assert!(manager.current().await.is_none());
        assert!(manager.snapshot().await.warning.is_some());
        server.abort();
    }

    #[tokio::test]
    async fn dropped_caller_and_cancelled_device_login_do_not_lose_rotation() {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let handler_started = started.clone();
        let handler_release = release.clone();
        let router = Router::new()
            .route("/token", post(move || { let started = handler_started.clone(); let release = handler_release.clone(); async move {
                started.notify_one();
                release.notified().await;
                Json(json!({"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":14400}))
            }}))
            .route("/validate", get(|| async { Json(json!({"client_id":"client","user_id":"123","login":"bot","scopes":["user:read:chat","user:write:chat"]})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let store = Arc::new(MemoryStore::default());
        let manager = AuthManager::with_store(
            reqwest::Client::new(),
            store.clone(),
            format!("http://{address}"),
        );
        manager.adopt(credential(), 0).await.unwrap();
        let generation = manager.current().await.unwrap().generation;
        let caller_manager = manager.clone();
        let caller =
            tokio::spawn(async move { caller_manager.refresh_after_401(generation).await });
        started.notified().await;
        caller.abort();
        manager.cancel_authorization();
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), manager.finish_pending())
            .await
            .unwrap();
        assert_eq!(
            store.value.lock().unwrap().as_ref().unwrap().refresh_token,
            "rotated-refresh"
        );
        assert_eq!(
            manager.current().await.unwrap().access_token,
            "rotated-access"
        );
        server.abort();
    }

    #[tokio::test]
    async fn offline_restore_keeps_credentials_private_until_validation() {
        let router = Router::new().route(
            "/validate",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let store = Arc::new(MemoryStore::default());
        *store.value.lock().unwrap() = Some(credential());
        let manager =
            AuthManager::with_store(reqwest::Client::new(), store, format!("http://{address}"));
        assert_eq!(
            manager.restore("client").await.unwrap_err(),
            AuthError::Network
        );
        assert!(manager.has_credentials().await);
        assert!(manager.current().await.is_none());
        assert!(!manager.snapshot().await.connected);
        server.abort();
    }

    #[test]
    fn secret_types_and_upstream_errors_are_redacted() {
        let session = AccessSession {
            access_token: "super-secret".into(),
            client_id: "client".into(),
            login: "bot".into(),
            user_id: "123".into(),
            scopes: vec![],
            generation: 1,
        };
        assert!(!format!("{session:?}").contains("super-secret"));
        let error = classify_error(
            reqwest::StatusCode::BAD_REQUEST,
            &json!({"message":"super-secret"}),
        );
        assert!(!error.to_string().contains("super-secret"));
        assert!(valid_verification_uri(
            "https://www.twitch.tv/activate?public=true&device-code=123"
        ));
        assert!(!valid_verification_uri("https://evil.example/activate"));
        assert!(!valid_verification_uri("http://www.twitch.tv/activate"));
    }
}
