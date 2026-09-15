//! Typed desktop boundary. No HTTP server, browser session, or polling UI.
use crate::{config::Config, diagnostics::LogEntry};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

pub enum AppCommand {
    SaveConfig(Box<Config>),
    Profile(ProfileAction),
    Preview(String),
    ConnectTwitch,
    CancelTwitch,
    DisconnectTwitch,
    Pause(bool),
    ClearMemory,
    ClearLogs,
    ExportLogs(PathBuf),
    Quit,
}

pub enum ProfileAction {
    Save {
        profile: crate::profiles::ApiProfile,
        key: Option<String>,
    },
    Activate(String),
    Delete(String),
    RemoveKey(String),
}

#[derive(Clone, Default)]
pub struct AuthView {
    pub status: String,
    pub login: String,
    pub user_code: String,
    pub verification_uri: String,
    pub connected: bool,
    pub pending: bool,
    pub persistent: bool,
}

#[derive(Clone)]
pub struct AppSnapshot {
    pub config: Config,
    pub auth: AuthView,
    pub profiles: Vec<crate::profiles::ProfileSummary>,
    pub active_profile_id: String,
    pub profile_revision: u64,
    pub profile_action_serial: u64,
    pub profile_error: Option<String>,
    pub profiles_ready: bool,
    pub twitch_status: String,
    pub busy: bool,
    pub notice: String,
    pub preview: String,
    pub logs: Vec<LogEntry>,
    pub log_dropped: u64,
    /// Changes only when the applied configuration changes.
    pub revision: u64,
}
impl Default for AppSnapshot {
    fn default() -> Self {
        Self {
            config: Config::default(),
            auth: AuthView::default(),
            profiles: Vec::new(),
            active_profile_id: String::new(),
            profile_revision: 0,
            profile_action_serial: 0,
            profile_error: None,
            profiles_ready: false,
            twitch_status: "Starting…".into(),
            busy: false,
            notice: String::new(),
            preview: String::new(),
            logs: Vec::new(),
            log_dropped: 0,
            revision: 0,
        }
    }
}

pub type WakeCallback = Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;
#[derive(Clone)]
pub struct DesktopHandle {
    pub commands: mpsc::Sender<AppCommand>,
    pub snapshot: Arc<Mutex<AppSnapshot>>,
    pub wake: WakeCallback,
}

use crate::{
    config,
    diagnostics::{Diagnostics, Event, Field, Level, Subsystem},
    engine::{self, ChatInput, Engine, PreparedTurn},
    profiles::Profiles,
    provider,
    secrets::Secrets,
    twitch::{
        self,
        auth::{AuthManager, AuthSnapshot},
    },
};
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, RwLock, Semaphore, watch},
    task::JoinSet,
};

pub struct App {
    pub config: RwLock<Config>,
    pub secrets: RwLock<Secrets>,
    profiles: RwLock<Option<Profiles>>,
    pub engine: AsyncMutex<Engine>,
    pub client: reqwest::Client,
    pub auth: Arc<AuthManager>,
    pub diagnostics: Diagnostics,
    pub changed: watch::Sender<u64>,
    pub legacy_twitch: bool,
    directory: PathBuf,
    desktop: DesktopHandle,
    revision: AtomicU64,
    pause_revision: AtomicU64,
    admission: Arc<Semaphore>,
    mutations: AsyncMutex<()>,
    login_gate: AsyncMutex<()>,
    ready: AtomicBool,
    disconnecting: AtomicBool,
    client_id: String,
}

pub struct GeneratedReply {
    pub reply: String,
    turn: PreparedTurn,
    config: Config,
    revision: u64,
    _busy: BusyGuard,
}
struct BusyGuard {
    app: Arc<App>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.app.update(|s| s.busy = false);
    }
}

impl App {
    fn update(&self, change: impl FnOnce(&mut AppSnapshot)) {
        if let Ok(mut snapshot) = self.desktop.snapshot.lock() {
            change(&mut snapshot);
        }
        if let Ok(wake) = self.desktop.wake.lock()
            && let Some(wake) = wake.as_ref()
        {
            wake();
        }
    }
    pub fn set_twitch_status(&self, status: String) {
        self.update(|s| s.twitch_status = status);
    }
    fn reject_command(&self, command: &AppCommand, error: &str) {
        if matches!(command, AppCommand::Profile(_)) {
            self.update(|s| {
                s.profile_action_serial = s.profile_action_serial.wrapping_add(1);
                s.profile_error = Some(error.into());
            });
        }
        self.notice(error);
    }
    fn notice(&self, message: impl Into<String>) {
        let text = message.into();
        self.update(|s| s.notice = text);
    }
    pub fn publish_logs(&self) {
        let logs = self.diagnostics.snapshot();
        let dropped = self.diagnostics.dropped();
        self.update(|s| {
            s.logs = logs;
            s.log_dropped = dropped;
        });
    }
    fn event(&self, level: Level, subsystem: Subsystem, event: Event) {
        self.diagnostics.emit(level, subsystem, event, &[]);
        self.publish_logs();
    }
    fn invalidate(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    async fn publish_config(&self) {
        let config = self.config.read().await.clone();
        let store = self.profiles.read().await;
        let profiles = store.as_ref().map(Profiles::summaries).unwrap_or_default();
        let active_id = store
            .as_ref()
            .map(|p| p.active_id().to_owned())
            .unwrap_or_default();
        let profiles_ready = store.is_some();
        drop(store);
        let revision = self.revision.load(Ordering::Relaxed);
        self.update(|s| {
            s.config = config;
            s.profiles = profiles;
            s.active_profile_id = active_id;
            s.profiles_ready = profiles_ready;
            s.revision = revision;
        });
        self.publish_logs();
    }
    fn show_auth(&self, auth: AuthSnapshot) {
        self.update(|s| update_auth_view(&mut s.auth, auth, false));
    }
    pub async fn refresh_auth_view(&self) {
        let auth = self.auth.snapshot().await;
        self.update(|s| update_auth_view(&mut s.auth, auth, true));
        self.publish_logs();
    }
    fn login_pending(&self) -> bool {
        self.desktop.snapshot.lock().is_ok_and(|s| s.auth.pending)
    }
    fn clear_pending_login(&self, status: &str) {
        self.update(|s| {
            s.auth.pending = false;
            s.auth.user_code.clear();
            s.auth.verification_uri.clear();
            s.auth.status = status.into();
        });
    }
    fn finish_login_view(&self, auth: AuthSnapshot) -> bool {
        let mut completed = false;
        self.update(|s| {
            // login_gate prevents a newer attempt starting before this one exits.
            // Cancel/disconnect clears pending synchronously, suppressing late results.
            if s.auth.pending {
                update_auth_view(&mut s.auth, auth, false);
                completed = true;
            }
        });
        completed
    }
    pub async fn generate(
        self: &Arc<Self>,
        input: ChatInput,
        preview: bool,
    ) -> Result<Option<GeneratedReply>, String> {
        let Ok(permit) = self.admission.clone().try_acquire_owned() else {
            return Ok(None);
        };
        let busy = BusyGuard {
            app: self.clone(),
            _permit: permit,
        };
        let mut changed = self.changed.subscribe();
        let revision = *changed.borrow_and_update();
        let Ok(state_gate) = self.mutations.try_lock() else {
            return Ok(None);
        };
        let mut config = self.config.read().await.clone();
        let key = {
            let store = self.profiles.read().await;
            let store = store
                .as_ref()
                .ok_or("API profiles could not be loaded. Check the profile storage notice.")?;
            store.active().apply(&mut config);
            store.key()
        };
        drop(state_gate);
        if key.is_empty() && config.provider != config::Provider::Compatible {
            return Err("Add an API key for the selected provider.".into());
        }
        let Some(turn) = self.engine.lock().await.prepare(&config, input, preview)? else {
            return Ok(None);
        };
        self.update(|s| s.busy = true);
        self.event(Level::Info, Subsystem::Provider, Event::RequestStarted);
        let start = Instant::now();
        let response = tokio::select! {
            _=changed.changed()=>return Err("Request cancelled because the bot settings or connection changed.".into()),
            response=provider::complete(&self.client,&config,&key,&turn.messages)=>response,
        };
        match response {
            Ok(text) => {
                let reply = engine::clean_reply(&text, config.max_reply_chars);
                if reply.is_empty() {
                    return Err("Provider returned an empty reply.".into());
                }
                self.diagnostics.emit(
                    Level::Info,
                    Subsystem::Provider,
                    Event::RequestSucceeded,
                    &[
                        Field::Provider(config.provider.id()),
                        Field::LatencyMs(start.elapsed().as_millis() as u64),
                    ],
                );
                self.publish_logs();
                Ok(Some(GeneratedReply {
                    reply,
                    turn,
                    config,
                    revision,
                    _busy: busy,
                }))
            }
            Err(error) => {
                error.log(&self.diagnostics);
                self.publish_logs();
                Err(error.to_string())
            }
        }
    }
    pub fn is_current(&self, generated: &GeneratedReply) -> bool {
        *self.changed.borrow() == generated.revision
    }
    pub async fn commit(self: &Arc<Self>, generated: GeneratedReply) {
        let mut engine = self.engine.lock().await;
        if self.is_current(&generated) {
            engine.delivered(&generated.config, generated.turn, generated.reply);
            drop(engine);
            self.event(Level::Info, Subsystem::Twitch, Event::ReplySent);
        }
    }
    async fn apply_config(&self, mut config: Config) -> Result<(), String> {
        let pause_revision = self.pause_revision.load(Ordering::Acquire);
        let _mutation = self
            .mutations
            .try_lock()
            .map_err(|_| "Another settings update is running. Try again shortly.")?;
        if let Some(store) = self.profiles.read().await.as_ref() {
            store.active().apply(&mut config);
        }
        config.validate()?;
        self.invalidate();
        let directory = self.directory.clone();
        let bytes = serde_json::to_vec_pretty(&config).map_err(|_| "Could not encode settings.")?;
        tokio::task::spawn_blocking(move || {
            config::atomic_write(&directory, "config.json", &bytes)
        })
        .await
        .map_err(|_| "Settings writer failed.")?
        .map_err(|_| "Could not save settings. Check directory permissions.")?;
        {
            let mut applied = self.config.write().await;
            if self.pause_revision.load(Ordering::Acquire) != pause_revision {
                config.enabled = applied.enabled;
            }
            *applied = config;
        }
        self.engine.lock().await.clear();
        self.revision.fetch_add(1, Ordering::Relaxed);
        self.invalidate();
        self.publish_config().await;
        self.event(Level::Info, Subsystem::Settings, Event::SettingsSaved);
        self.notice("Settings saved.");
        Ok(())
    }
    async fn update_profile(&self, action: ProfileAction) -> Result<(), String> {
        let _mutation = self
            .mutations
            .try_lock()
            .map_err(|_| "Another settings update is running. Try again shortly.")?;
        let mut next =
            self.profiles.read().await.clone().ok_or(
                "API profiles could not be loaded. Restore the profile file before editing.",
            )?;
        let credential_event = match &action {
            ProfileAction::Save { key: Some(_), .. } => Some(Event::CredentialSaved),
            ProfileAction::RemoveKey(_) | ProfileAction::Delete(_) => {
                Some(Event::CredentialRemoved)
            }
            _ => None,
        };
        match action {
            ProfileAction::Save { profile, key } => next.save(profile, key)?,
            ProfileAction::Activate(id) => next.activate(&id)?,
            ProfileAction::Delete(id) => next.delete(&id)?,
            ProfileAction::RemoveKey(id) => next.remove_key(&id)?,
        }
        // Invalidate before I/O. Keep the current profile/key pair untouched on failure.
        self.invalidate();
        let directory = self.directory.clone();
        let next = tokio::task::spawn_blocking(move || {
            next.persist(&directory)?;
            Ok::<_, String>(next)
        })
        .await
        .map_err(|_| "API profile writer failed.")??;
        next.active().apply(&mut *self.config.write().await);
        *self.profiles.write().await = Some(next);
        self.engine.lock().await.clear();
        self.revision.fetch_add(1, Ordering::Relaxed);
        self.update(|s| s.profile_revision = s.profile_revision.wrapping_add(1));
        self.invalidate();
        self.publish_config().await;
        self.event(Level::Info, Subsystem::Settings, Event::SettingsSaved);
        if let Some(event) = credential_event {
            self.event(Level::Info, Subsystem::Credentials, event);
        }
        self.notice("API profiles saved. The selected profile is now active.");
        Ok(())
    }
    async fn connect(self: &Arc<Self>) -> Result<(), String> {
        if self.disconnecting.load(Ordering::Acquire) {
            return Err("Twitch is disconnecting. Try Connect again when it finishes.".into());
        }
        let _login = self
            .login_gate
            .try_lock()
            .map_err(|_| "Twitch sign-in is already in progress. Cancel it before trying again.")?;
        self.invalidate();
        if self.client_id.is_empty() {
            return Err("Twitch sign-in is awaiting MPD Bot's public app registration. The developer must supply its Client ID in this build.".into());
        }
        self.update(|s| {
            s.auth.pending = true;
            s.auth.status = "Requesting Twitch authorization...".into();
            s.auth.user_code.clear();
            s.auth.verification_uri.clear();
        });
        let grant = match self.auth.begin_device(&self.client_id).await {
            Ok(grant) => grant,
            Err(error) => return self.finish_login_error(error).await,
        };
        if !self.login_pending() {
            return Ok(());
        }
        self.update(|s| {
            s.auth.status = format!(
                "Authorize on Twitch within {} minutes.",
                grant.expires_in.div_ceil(60)
            );
            s.auth.user_code = grant.user_code.clone();
            s.auth.verification_uri = grant.verification_uri.clone();
        });
        self.event(Level::Info, Subsystem::OAuth, Event::AuthorizationStarted);
        let url = grant.verification_uri.clone();
        // The URL is produced and restricted to Twitch by the auth manager.
        let opened = tokio::task::spawn_blocking(move || webbrowser::open(&url))
            .await
            .is_ok_and(|r| r.is_ok());
        if !self.login_pending() {
            return Ok(());
        }
        if !opened {
            self.notice(
                "The browser could not be opened. Use the authorization link in Twitch connection.",
            );
        }
        match self.auth.poll_device(grant).await {
            Ok(auth) => {
                if !self.finish_login_view(auth) {
                    return Ok(());
                }
                self.invalidate();
                self.event(Level::Info, Subsystem::OAuth, Event::Authorized);
                self.notice("Twitch account connected. Choose the channel and enable its connection in settings.");
                Ok(())
            }
            Err(error) => self.finish_login_error(error).await,
        }
    }
    async fn finish_login_error(&self, error: twitch::auth::AuthError) -> Result<(), String> {
        if !self.finish_login_view(self.auth.snapshot().await) {
            return Ok(());
        }
        if error == twitch::auth::AuthError::Cancelled {
            return Ok(());
        }
        self.event(
            Level::Warn,
            Subsystem::OAuth,
            match error {
                twitch::auth::AuthError::AuthorizationExpired => Event::AuthorizationExpired,
                twitch::auth::AuthError::Reauthorize => Event::ReconnectRequired,
                _ => Event::AuthorizationFailed,
            },
        );
        Err(error.to_string())
    }
    async fn command(self: Arc<Self>, command: AppCommand) -> Result<(), String> {
        match command {
            AppCommand::SaveConfig(config) => self.apply_config(*config).await.inspect_err(|_| {
                self.event(Level::Warn, Subsystem::Settings, Event::SettingsRejected);
            }),
            AppCommand::Profile(action) => {
                let result = self.update_profile(action).await;
                self.update(|s| {
                    s.profile_action_serial = s.profile_action_serial.wrapping_add(1);
                    s.profile_error = result.as_ref().err().cloned();
                });
                if result.is_err() {
                    self.event(Level::Warn, Subsystem::Settings, Event::SettingsRejected);
                }
                result
            }
            AppCommand::Preview(message) => {
                let input = ChatInput {
                    platform: "preview".into(),
                    channel: "preview".into(),
                    user: "you".into(),
                    message,
                };
                self.update(|s| s.preview = "Generating…".into());
                match self.generate(input, true).await {
                    Ok(Some(reply)) => {
                        if self.is_current(&reply) {
                            self.update(|s| s.preview = reply.reply.clone());
                        }
                        Ok(())
                    }
                    Ok(None) => {
                        self.update(|s| s.preview = "The bot is busy. Try again shortly.".into());
                        Ok(())
                    }
                    Err(error) => {
                        self.update(|s| s.preview = error.clone());
                        Err(error)
                    }
                }
            }
            AppCommand::ConnectTwitch => self.connect().await,
            AppCommand::CancelTwitch => {
                self.auth.cancel_authorization();
                self.clear_pending_login("Twitch authorization cancelled.");
                self.refresh_auth_view().await;
                self.event(Level::Info, Subsystem::OAuth, Event::AuthorizationCancelled);
                Ok(())
            }
            AppCommand::DisconnectTwitch => {
                self.disconnecting.store(true, Ordering::Release);
                self.invalidate();
                self.auth.cancel_authorization();
                self.clear_pending_login("Disconnecting Twitch...");
                let result = self.auth.logout().await;
                self.disconnecting.store(false, Ordering::Release);
                self.invalidate();
                self.refresh_auth_view().await;
                self.event(Level::Info, Subsystem::OAuth, Event::Disconnected);
                result.map_err(|e| e.to_string())
            }
            AppCommand::Pause(paused) => {
                self.pause_revision.fetch_add(1, Ordering::AcqRel);
                self.config.write().await.enabled = !paused;
                self.invalidate();
                self.revision.fetch_add(1, Ordering::Relaxed);
                self.publish_config().await;
                self.event(
                    Level::Info,
                    Subsystem::App,
                    if paused {
                        Event::Paused
                    } else {
                        Event::Resumed
                    },
                );
                Ok(())
            }
            AppCommand::ClearMemory => {
                self.invalidate();
                self.engine.lock().await.clear();
                self.event(Level::Info, Subsystem::App, Event::MemoryCleared);
                self.notice("Conversation memory cleared.");
                Ok(())
            }
            AppCommand::ClearLogs => {
                self.diagnostics.clear();
                self.event(Level::Info, Subsystem::App, Event::LogsCleared);
                Ok(())
            }
            AppCommand::ExportLogs(path) => {
                let report = self.diagnostics.export();
                tokio::task::spawn_blocking(move || std::fs::write(path, report))
                    .await
                    .map_err(|_| "Log export failed.")?
                    .map_err(|_| "Could not write the log export.")?;
                self.notice("Sanitized session logs exported.");
                Ok(())
            }
            AppCommand::Quit => Ok(()),
        }
    }
}

// A background token check updates account metadata without replacing an active
// device authorization instruction/code. Only explicit completion/cancel clears it.
fn update_auth_view(view: &mut AuthView, auth: AuthSnapshot, preserve_pending: bool) {
    let pending = preserve_pending && view.pending;
    view.login = auth.login.unwrap_or_default();
    view.connected = auth.connected;
    view.persistent = auth.persistent;
    if !pending {
        view.status = auth.warning.unwrap_or_else(|| {
            if auth.connected {
                "Connected".into()
            } else {
                "Not connected".into()
            }
        });
        view.pending = false;
        view.user_code.clear();
        view.verification_uri.clear();
    }
}

pub struct RuntimeWorker {
    commands: mpsc::Sender<AppCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl RuntimeWorker {
    pub fn shutdown(mut self) {
        let _ = self.commands.blocking_send(AppCommand::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts one network runtime thread. The main thread remains owned by the UI.
pub fn start(directory: PathBuf, config: Config, demo: bool) -> (DesktopHandle, RuntimeWorker) {
    let (commands, receiver) = mpsc::channel(32);
    let desktop = DesktopHandle {
        commands: commands.clone(),
        snapshot: Arc::new(Mutex::new(AppSnapshot {
            config: config.clone(),
            ..AppSnapshot::default()
        })),
        wake: Arc::new(Mutex::new(None)),
    };
    let ui = desktop.clone();
    let thread = std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                if let Ok(mut s) = ui.snapshot.lock() {
                    s.notice = "Could not start application runtime.".into();
                }
                return;
            }
        };
        runtime.block_on(run(directory, config, ui, receiver, demo));
        runtime.shutdown_timeout(Duration::from_secs(2));
    });
    (
        desktop,
        RuntimeWorker {
            commands,
            thread: Some(thread),
        },
    )
}
async fn run(
    directory: PathBuf,
    config: Config,
    desktop: DesktopHandle,
    mut receiver: mpsc::Receiver<AppCommand>,
    demo: bool,
) {
    let client = match provider::client() {
        Ok(client) => client,
        Err(_) => {
            if let Ok(mut s) = desktop.snapshot.lock() {
                s.notice = "Could not initialize secure networking.".into();
            }
            return;
        }
    };
    let diagnostics = Diagnostics::new();
    let (changed, changes) = watch::channel(1);
    let app = Arc::new(App {
        profiles: RwLock::new(if demo {
            Some(Profiles::from_legacy(
                &config,
                &Secrets {
                    values: Default::default(),
                    warning: None,
                },
            ))
        } else {
            None
        }),
        config: RwLock::new(config),
        secrets: RwLock::new(Secrets {
            values: Default::default(),
            warning: None,
        }),
        engine: AsyncMutex::new(Engine::default()),
        auth: Arc::new(AuthManager::new(client.clone(), directory.clone())),
        client,
        diagnostics,
        changed,
        directory,
        desktop,
        revision: AtomicU64::new(1),
        pause_revision: AtomicU64::new(0),
        admission: Arc::new(Semaphore::new(1)),
        mutations: AsyncMutex::new(()),
        login_gate: AsyncMutex::new(()),
        ready: AtomicBool::new(demo),
        disconnecting: AtomicBool::new(false),
        legacy_twitch: !demo && std::env::var("MPD_BOT_LEGACY_TWITCH").is_ok_and(|s| s == "1"),
        client_id: if demo {
            String::new()
        } else {
            std::env::var("MPD_BOT_TWITCH_CLIENT_ID")
                .ok()
                .or_else(|| option_env!("MPD_BOT_TWITCH_CLIENT_ID").map(str::to_string))
                .unwrap_or_default()
        },
    });
    app.event(Level::Info, Subsystem::App, Event::Started);
    if app.directory.join("last-crash.txt").exists() {
        app.event(Level::Warn, Subsystem::App, Event::PreviousCrash);
    }
    let mut jobs = JoinSet::new();
    if !demo {
        let init = app.clone();
        jobs.spawn(async move {
            let credential_directory = init.directory.clone();
            let legacy_config = init.config.read().await.clone();
            match tokio::task::spawn_blocking(move || {
                let mut keys = Secrets::load(
                    &credential_directory,
                    !Profiles::has_file(&credential_directory),
                );
                let profiles =
                    Profiles::load_or_migrate(&credential_directory, &legacy_config, &keys);
                // Provider keys now belong exclusively to named profiles. Keep only legacy Twitch auth.
                keys.values.retain(|id, _| id == "twitch");
                (keys, profiles)
            })
            .await
            {
                Ok((keys, profiles)) => {
                    *init.secrets.write().await = keys;
                    match profiles {
                        Ok(profiles) => {
                            profiles.active().apply(&mut *init.config.write().await);
                            *init.profiles.write().await = Some(profiles);
                            init.revision.fetch_add(1, Ordering::Relaxed);
                            init.update(|s| s.profile_revision += 1);
                        }
                        Err(error) => {
                            init.notice(error);
                            init.event(
                                Level::Warn,
                                Subsystem::Credentials,
                                Event::CredentialStoreUnavailable,
                            );
                        }
                    }
                }
                Err(_) => init.notice("Could not load API profiles. Restart MPD Bot to try again."),
            }
            init.publish_config().await;
            if !init.client_id.is_empty() {
                match init.auth.restore(&init.client_id).await {
                    Ok(auth) => init.show_auth(auth),
                    Err(error) => {
                        init.notice(error.to_string());
                        init.event(Level::Warn, Subsystem::OAuth, Event::AuthorizationFailed);
                    }
                }
            } else {
                init.update(|s| {
                    s.auth.status = "Sign-in available after MPD Bot app registration.".into()
                });
            }
            init.ready.store(true, Ordering::Release);
            init.invalidate();
        });
    } else {
        app.notice("Demo mode: no saved credentials or Twitch connection loaded.");
    }
    let twitch_task = if demo {
        None
    } else {
        Some(tokio::spawn(twitch::run(app.clone(), changes)))
    };
    let maintenance_app = app.clone();
    let maintenance = tokio::spawn(async move {
        let mut delay = Duration::from_secs(30);
        loop {
            tokio::time::sleep(delay).await;
            if !maintenance_app.auth.has_credentials().await {
                delay = Duration::from_secs(30);
                continue;
            }
            let before = maintenance_app.auth.current().await;
            let was_connected = before.is_some();
            match maintenance_app.auth.validate().await {
                Ok(_) => {
                    let after = maintenance_app.auth.current().await;
                    if before
                        .as_ref()
                        .zip(after.as_ref())
                        .is_some_and(|(a, b)| a.generation != b.generation)
                    {
                        maintenance_app.event(Level::Info, Subsystem::OAuth, Event::TokenRefreshed);
                    }
                    maintenance_app.refresh_auth_view().await;
                    if !was_connected {
                        maintenance_app.invalidate();
                    }
                    delay = Duration::from_secs(3600);
                }
                Err(error) => {
                    maintenance_app.notice(error.to_string());
                    maintenance_app.refresh_auth_view().await;
                    if maintenance_app.auth.current().await.is_none() {
                        maintenance_app.invalidate();
                    }
                    maintenance_app.event(
                        Level::Warn,
                        Subsystem::OAuth,
                        Event::AuthorizationFailed,
                    );
                    delay = Duration::from_secs(30);
                }
            }
        }
    });
    app.publish_config().await;
    // Diagnostics producers never call the UI while holding the log lock. Application
    // event boundaries publish snapshots; the window itself never polls.
    loop {
        tokio::select! {
            command=receiver.recv()=> {
                let Some(command)=command else {break};
                if matches!(command,AppCommand::Quit) {break;}

                if !app.ready.load(Ordering::Acquire) {app.reject_command(&command, "Loading saved credentials. Try this action again shortly.");continue;}
                let priority = matches!(command, AppCommand::CancelTwitch | AppCommand::DisconnectTwitch | AppCommand::Pause(true));
                if jobs.len() >= if priority {8} else {6} {app.reject_command(&command, "The application is busy. Your action was not applied; try again shortly.");continue;}
                if matches!(command, AppCommand::CancelTwitch) {
                    if let Err(error)=app.clone().command(command).await {app.notice(error);}
                    continue;
                }
                if matches!(command, AppCommand::DisconnectTwitch) {
                    if app.disconnecting.swap(true, Ordering::AcqRel) {continue;}
                    app.auth.cancel_authorization();
                    app.clear_pending_login("Disconnecting Twitch...");
                    app.invalidate();
                }
                let action=app.clone(); jobs.spawn(async move {if let Err(error)=action.clone().command(command).await{action.notice(error);}});
            }
            _=jobs.join_next(),if !jobs.is_empty()=>{},
        }
    }
    app.invalidate();
    app.auth.cancel_authorization();
    app.event(Level::Info, Subsystem::App, Event::Stopping);
    if let Some(task) = twitch_task {
        task.abort();
        let _ = task.await;
    }
    maintenance.abort();
    let _ = maintenance.await;
    jobs.abort_all();
    while jobs.join_next().await.is_some() {}
    let _ = tokio::time::timeout(Duration::from_secs(25), app.auth.finish_pending()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(directory: PathBuf) -> Arc<App> {
        let config = Config {
            model: "mock".into(),
            provider: config::Provider::Compatible,
            endpoint: "http://127.0.0.1:1234/v1/chat/completions".into(),
            ..Config::default()
        };
        let (commands, _receiver) = mpsc::channel(32);
        let desktop = DesktopHandle {
            commands,
            snapshot: Arc::new(Mutex::new(AppSnapshot {
                config: config.clone(),
                ..AppSnapshot::default()
            })),
            wake: Arc::new(Mutex::new(None)),
        };
        let client = provider::client().unwrap();
        Arc::new(App {
            profiles: RwLock::new(Some(Profiles::from_legacy(
                &config,
                &Secrets {
                    values: Default::default(),
                    warning: None,
                },
            ))),
            config: RwLock::new(config),
            secrets: RwLock::new(Secrets {
                values: Default::default(),
                warning: None,
            }),
            engine: AsyncMutex::new(Engine::default()),
            auth: Arc::new(AuthManager::new(client.clone(), directory.clone())),
            client,
            diagnostics: Diagnostics::new(),
            changed: watch::channel(1).0,
            directory,
            desktop,
            revision: AtomicU64::new(1),
            pause_revision: AtomicU64::new(0),
            admission: Arc::new(Semaphore::new(1)),
            mutations: AsyncMutex::new(()),
            login_gate: AsyncMutex::new(()),
            ready: AtomicBool::new(true),
            disconnecting: AtomicBool::new(false),
            legacy_twitch: false,
            client_id: String::new(),
        })
    }
    #[tokio::test]
    async fn selected_profile_controls_outgoing_model_and_key() {
        use axum::{Json, Router, http::HeaderMap, routing::post};
        let (sent, mut received) = tokio::sync::mpsc::channel(4);
        let router = Router::new().route("/chat", post(move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
            let sent = sent.clone();
            async move {
                sent.send((headers["authorization"].to_str().unwrap().to_string(), body["model"].as_str().unwrap().to_string())).await.unwrap();
                Json(serde_json::json!({"choices":[{"message":{"content":"Synthetic reply"}}]}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path().into());
        let mut ids = Vec::new();
        for (name, model, key) in [
            ("Fast", "model-one", "synthetic-one"),
            ("Creative", "model-two", "synthetic-two"),
        ] {
            app.update_profile(ProfileAction::Save {
                profile: crate::profiles::ApiProfile {
                    id: String::new(),
                    name: name.into(),
                    provider: config::Provider::Compatible,
                    model: model.into(),
                    endpoint: endpoint.clone(),
                },
                key: Some(key.into()),
            })
            .await
            .unwrap();
            ids.push(
                app.profiles
                    .read()
                    .await
                    .as_ref()
                    .unwrap()
                    .active_id()
                    .to_owned(),
            );
        }
        for (index, model, key) in [
            (0, "model-one", "synthetic-one"),
            (1, "model-two", "synthetic-two"),
            (0, "model-one", "synthetic-one"),
        ] {
            app.update_profile(ProfileAction::Activate(ids[index].clone()))
                .await
                .unwrap();
            let reply = app
                .clone()
                .generate(
                    ChatInput {
                        platform: "preview".into(),
                        channel: "test".into(),
                        user: "test".into(),
                        message: "hello".into(),
                    },
                    true,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(reply.reply, "Synthetic reply");
            drop(reply);
            let request = received.recv().await.unwrap();
            assert_eq!(request, (format!("Bearer {key}"), model.into()));
        }
        server.abort();
        assert!(!app.diagnostics.export().contains("synthetic-one"));
    }
    #[tokio::test]
    async fn failed_profile_write_preserves_applied_profile_and_reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path().into());
        std::fs::write(dir.path().join("credentials"), "blocks-directory-creation").unwrap();
        let original = app.profiles.read().await.as_ref().unwrap().active().clone();
        let revision = *app.changed.borrow();
        let mut edited = original.clone();
        edited.model = "changed-model".into();
        let result = app
            .clone()
            .command(AppCommand::Profile(ProfileAction::Save {
                profile: edited,
                key: Some("synthetic-key".into()),
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(
            *app.profiles.read().await.as_ref().unwrap().active(),
            original
        );
        assert!(*app.changed.borrow() > revision);
        let snapshot = app.desktop.snapshot.lock().unwrap();
        assert_eq!(snapshot.profile_action_serial, 1);
        assert!(snapshot.profile_error.is_some());
    }
    #[tokio::test]
    async fn saved_config_keeps_secrets_separate_and_rejects_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path().into());
        let config = Config {
            model: "mock".into(),
            bot_name: "Native test".into(),
            ..Config::default()
        };
        app.apply_config(config.clone()).await.unwrap();
        assert_eq!(config::load(dir.path()).unwrap().bot_name, "Native test");
        assert_eq!(
            app.desktop.snapshot.lock().unwrap().config.bot_name,
            "Native test"
        );
        let invalid = Config {
            max_output_tokens: 0,
            ..config
        };
        assert!(app.apply_config(invalid).await.is_err());
        assert_eq!(config::load(dir.path()).unwrap().max_output_tokens, 1024);
        let mut profile = app.profiles.read().await.as_ref().unwrap().active().clone();
        profile.provider = config::Provider::Anthropic;
        profile.endpoint.clear();
        app.update_profile(ProfileAction::Save {
            profile,
            key: Some("synthetic-private-key".into()),
        })
        .await
        .unwrap();
        assert!(!app.diagnostics.export().contains("synthetic-private-key"));
        assert!(
            !std::fs::read_to_string(dir.path().join("config.json"))
                .unwrap()
                .contains("synthetic-private-key")
        );
        assert!(
            !app.desktop
                .snapshot
                .lock()
                .unwrap()
                .profiles
                .iter()
                .map(|p| format!("{:?}", p.profile))
                .collect::<String>()
                .contains("synthetic-private-key")
        );
    }
    #[tokio::test]
    async fn pause_cancels_slow_generation_without_waiting_for_provider() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let (seen, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 4096];
            let _ = socket.read(&mut bytes).await;
            let _ = seen.send(());
            std::future::pending::<()>().await;
        });
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path().into());
        app.config.write().await.endpoint = endpoint;
        let updated = app.config.read().await.clone();
        *app.profiles.write().await = Some(Profiles::from_legacy(
            &updated,
            &Secrets {
                values: Default::default(),
                warning: None,
            },
        ));
        let request = app.clone();
        let generate = tokio::spawn(async move {
            request
                .generate(
                    ChatInput {
                        platform: "preview".into(),
                        channel: "preview".into(),
                        user: "you".into(),
                        message: "synthetic message".into(),
                    },
                    true,
                )
                .await
        });
        received.await.unwrap();
        app.clone().command(AppCommand::Pause(true)).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), generate)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert!(!app.desktop.snapshot.lock().unwrap().busy);
        assert_eq!(app.admission.available_permits(), 1);
        assert_eq!(app.engine.lock().await.count(), 0);
        server.abort();
    }
    #[tokio::test]
    async fn pause_does_not_wait_for_settings_or_credential_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path().into());
        let gate = app.mutations.lock().await;
        tokio::time::timeout(
            Duration::from_millis(100),
            app.clone().command(AppCommand::Pause(true)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!app.config.read().await.enabled);
        assert!(!app.desktop.snapshot.lock().unwrap().config.enabled);
        drop(gate);
    }
    #[tokio::test]
    async fn missing_registration_is_clear_and_does_not_start_login() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path().into());
        assert!(
            app.clone()
                .command(AppCommand::ConnectTwitch)
                .await
                .unwrap_err()
                .contains("registration")
        );
        assert!(!app.desktop.snapshot.lock().unwrap().auth.pending);
    }
    fn pending_login(app: &App) {
        app.update(|s| {
            s.auth = AuthView {
                status: "Authorize on Twitch within 1 minute.".into(),
                user_code: "TESTCODE".into(),
                verification_uri: "https://www.twitch.tv/activate?device-code=TESTCODE".into(),
                pending: true,
                connected: true,
                login: "previous-account".into(),
                persistent: true,
            };
        });
    }

    #[tokio::test]
    async fn background_auth_refresh_keeps_pending_device_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path().into());
        pending_login(&app);
        app.refresh_auth_view().await;
        let snapshot = app.desktop.snapshot.lock().unwrap();
        assert!(snapshot.auth.pending);
        assert_eq!(snapshot.auth.user_code, "TESTCODE");
        assert!(snapshot.auth.verification_uri.contains("TESTCODE"));
        assert_eq!(snapshot.auth.status, "Authorize on Twitch within 1 minute.");
        // Current account details still update even while a new login is pending.
        assert!(!snapshot.auth.connected);
        assert!(!snapshot.auth.persistent);
        assert!(snapshot.auth.login.is_empty());
    }

    #[tokio::test]
    async fn cancel_and_disconnect_clear_codes_and_ignore_late_login_results() {
        for command in [AppCommand::CancelTwitch, AppCommand::DisconnectTwitch] {
            let dir = tempfile::tempdir().unwrap();
            let app = app(dir.path().into());
            pending_login(&app);
            app.clone().command(command).await.unwrap();
            let logged = app.diagnostics.snapshot().len();
            assert!(!app.finish_login_view(AuthSnapshot {
                connected: true,
                login: Some("late-account".into()),
                persistent: true,
                warning: None,
            }));
            app.finish_login_error(twitch::auth::AuthError::Cancelled)
                .await
                .unwrap();
            app.finish_login_error(twitch::auth::AuthError::Network)
                .await
                .unwrap();
            assert_eq!(app.diagnostics.snapshot().len(), logged);
            let snapshot = app.desktop.snapshot.lock().unwrap();
            assert!(!snapshot.auth.pending);
            assert!(!snapshot.auth.connected);
            assert!(snapshot.auth.user_code.is_empty());
            assert!(snapshot.auth.verification_uri.is_empty());
            assert_ne!(snapshot.auth.login, "late-account");
        }
    }

    #[tokio::test]
    async fn successful_login_explicitly_finishes_pending_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path().into());
        pending_login(&app);
        assert!(app.finish_login_view(AuthSnapshot {
            connected: true,
            login: Some("authorized-bot".into()),
            persistent: true,
            warning: None,
        }));
        let snapshot = app.desktop.snapshot.lock().unwrap();
        assert!(snapshot.auth.connected);
        assert!(!snapshot.auth.pending);
        assert_eq!(snapshot.auth.login, "authorized-bot");
        assert!(snapshot.auth.user_code.is_empty());
        assert!(snapshot.auth.verification_uri.is_empty());
    }
}
