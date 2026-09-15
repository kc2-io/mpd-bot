use mpd_bot_view::*;
use slint::{ComponentHandle, winit_030::WinitWindowAccessor};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

static TRAY_FAILED: AtomicBool = AtomicBool::new(false);
static TRAY_FAILURE_WAKE: Mutex<Option<Box<dyn Fn() + Send + Sync>>> = Mutex::new(None);
static TRAY_LOGGER: TrayLogger = TrayLogger;
struct TrayLogger;
impl log::Log for TrayLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("i_slint_core::debug_log")
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Match only a bounded fixed prefix. Never retain arbitrary framework log text.
        struct Prefix(String);
        impl std::fmt::Write for Prefix {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                for ch in text.chars() {
                    if self.0.len() + ch.len_utf8() > 80 {
                        break;
                    }
                    self.0.push(ch);
                }
                Ok(())
            }
        }
        let mut prefix = Prefix(String::new());
        let _ = std::fmt::write(&mut prefix, *record.args());
        if prefix
            .0
            .starts_with("Slint: Failed to create system tray icon:")
        {
            TRAY_FAILED.store(true, Ordering::Release);
            if let Ok(wake) = TRAY_FAILURE_WAKE.lock()
                && let Some(wake) = wake.as_ref()
            {
                wake();
            }
        }
    }
    fn flush(&self) {}
}

fn initialize_backend() -> Result<(), slint::PlatformError> {
    if log::set_logger(&TRAY_LOGGER).is_err() {
        TRAY_FAILED.store(true, Ordering::Release);
    }
    log::set_max_level(log::LevelFilter::Debug);
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()
}

fn install_tray(window: &DesktopWindow) -> Option<BotTray> {
    let weak = window.as_weak();
    if let Ok(mut wake) = TRAY_FAILURE_WAKE.lock() {
        *wake = Some(Box::new(move || {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(window) = weak.upgrade() {
                    window.set_tray_available(false);
                    window.set_notice(
                        "The system tray is unavailable. Keep this window open; use Quit to exit."
                            .into(),
                    );
                    restore(&window);
                }
            });
        }));
    }
    let tray = BotTray::new().ok().filter(|t| t.show().is_ok());
    window.set_tray_available(tray.is_some() && !TRAY_FAILED.load(Ordering::Acquire));
    if let Some(tray) = &tray {
        let weak = window.as_weak();
        tray.on_open_window(move || {
            if let Some(window) = weak.upgrade() {
                restore(&window);
            }
        });
        let weak = window.as_weak();
        tray.on_quit(move || {
            if let Some(window) = weak.upgrade() {
                restore(&window);
                window.invoke_quit();
            }
        });
    }
    let weak = window.as_weak();
    window.on_hide_to_tray(move || {
        if let Some(window) = weak.upgrade()
            && window.get_tray_available()
            && !TRAY_FAILED.load(Ordering::Acquire)
        {
            let _ = window.hide();
        }
    });
    let weak = window.as_weak();
    window.window().on_close_requested(move || {
        if let Some(window) = weak.upgrade() {
            if window.get_tray_available() && !TRAY_FAILED.load(Ordering::Acquire) {
                return slint::CloseRequestResponse::HideWindow;
            }
            window.invoke_quit();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    let weak = window.as_weak();
    window
        .window()
        .on_winit_window_event(move |window, _event| {
            if let Some(ui) = weak.upgrade()
                && ui.get_tray_available()
                && !TRAY_FAILED.load(Ordering::Acquire)
                && window
                    .with_winit_window(|w| w.is_minimized().unwrap_or(false))
                    .unwrap_or(false)
            {
                let _ = window.hide();
            }
            slint::winit_030::EventResult::Propagate
        });
    tray
}

mod smoke;
/// Isolated lifecycle experiment. Never initializes credentials or networking.
pub fn run_spike() -> Result<(), Box<dyn std::error::Error>> {
    smoke::run()
}

fn restore(window: &DesktopWindow) {
    window.window().set_minimized(false);
    let _ = window.show();
    window.window().with_winit_window(|window| {
        window.set_minimized(false);
        window.focus_window();
    });
}

use crate::{
    application::{AppCommand, AppSnapshot, DesktopHandle},
    config::{Config, Provider},
};
use std::sync::Arc;

#[derive(Default)]
struct ViewCache {
    revision: Option<u64>,
    previous_enabled: bool,
    pending_save: Option<Config>,
    notice: String,
    log_fingerprint: String,
    pending_edit_revision: i32,
    pending_config_revision: u64,
}

fn install_memory_estimate(window: &DesktopWindow) {
    window.on_memory_estimate(|messages, viewers, reply_chars| {
        let bytes = crate::engine::estimated_chat_memory(
            messages.max(0) as usize,
            viewers.max(0) as usize,
            reply_chars.max(0) as usize,
        );
        if bytes == 0 {
            "Estimated chat memory: 0 MiB (memory off)".into()
        } else {
            format!(
                "Estimated full chat memory: ~{:.2} MiB",
                bytes as f64 / (1024.0 * 1024.0)
            )
            .into()
        }
    });
}

fn view_settings(config: &Config) -> SettingsView {
    SettingsView {
        enabled: config.enabled,
        twitch_enabled: config.twitch_enabled,
        twitch_username: config.twitch_username.clone().into(),
        twitch_channel: config.twitch_channel.clone().into(),
        provider: Provider::ALL
            .iter()
            .position(|p| *p == config.provider)
            .unwrap_or(0) as i32,
        model: config.model.clone().into(),
        endpoint: config.endpoint.clone().into(),
        bot_name: config.bot_name.clone().into(),
        personality: config.personality.clone().into(),
        prompt: config.prompt.clone().into(),
        command: config.command.clone().into(),
        command_enabled: config.command_enabled,
        respond_to_mentions: config.respond_to_mentions,
        random_reply_percent: config.random_reply_percent as i32,
        excluded_users: config.excluded_users.join(", ").into(),
        cooldown_seconds: config.cooldown_seconds as i32,
        remembered_messages: config.memory_turns as i32,
        max_conversations: config.max_conversations as i32,
        max_output_tokens: config.max_output_tokens as i32,
        max_reply_chars: config.max_reply_chars as i32,
    }
}
fn settings_config(view: &SettingsView, base: &Config) -> Config {
    let mut config = base.clone();
    config.enabled = view.enabled;
    config.twitch_enabled = view.twitch_enabled;
    config.twitch_channel = view.twitch_channel.trim().to_ascii_lowercase();
    config.provider = Provider::ALL
        .get(view.provider as usize)
        .copied()
        .unwrap_or(Provider::Openai);
    config.model = view.model.trim().into();
    config.endpoint = view.endpoint.trim().into();
    config.bot_name = view.bot_name.trim().into();
    config.personality = view.personality.to_string();
    config.prompt = view.prompt.to_string();
    config.command = view.command.trim().into();
    config.command_enabled = view.command_enabled;
    config.respond_to_mentions = view.respond_to_mentions;
    config.random_reply_percent = view.random_reply_percent.max(0) as u32;
    config.excluded_users = view
        .excluded_users
        .split([',', '\n'])
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    config.cooldown_seconds = view.cooldown_seconds.max(0) as u64;
    config.memory_turns = view.remembered_messages.max(0) as usize;
    config.max_conversations = view.max_conversations.max(0) as usize;
    config.max_output_tokens = view.max_output_tokens.max(0) as u32;
    config.max_reply_chars = view.max_reply_chars.max(0) as usize;
    config
}
fn same_config(left: &Config, right: &Config) -> bool {
    serde_json::to_value(left).ok() == serde_json::to_value(right).ok()
}

fn submit(window: &DesktopWindow, handle: &DesktopHandle, command: AppCommand) -> bool {
    match handle.commands.try_send(command) {
        Ok(()) => true,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            window.set_notice("The app is busy applying another action. Try again shortly.".into());
            false
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            window.set_notice("The app runtime has stopped. Quit and restart MPD Bot.".into());
            false
        }
    }
}

fn render(
    window: &DesktopWindow,
    tray: Option<&BotTray>,
    snapshot: &AppSnapshot,
    cache: &mut ViewCache,
    force_logs: bool,
) {
    if cache.revision.is_none()
        || (!window.get_dirty() && cache.revision != Some(snapshot.revision))
    {
        window.set_settings(view_settings(&snapshot.config));
    } else if cache.revision != Some(snapshot.revision) {
        let mut settings = window.get_settings();
        // A tray pause remains authoritative unless the user explicitly edited this checkbox.
        if settings.enabled == cache.previous_enabled {
            settings.enabled = snapshot.config.enabled;
        }
        settings.twitch_username = snapshot.config.twitch_username.clone().into();
        window.set_settings(settings);
    }
    if let Some(pending) = &cache.pending_save {
        // A later tray pause or validated account identity must survive the settings write.
        let mut applied = pending.clone();
        applied.enabled = snapshot.config.enabled;
        applied.twitch_username = snapshot.config.twitch_username.clone();
        if snapshot.revision > cache.pending_config_revision
            && same_config(&applied, &snapshot.config)
        {
            if window.get_edit_revision() == cache.pending_edit_revision {
                window.set_dirty(false);
                window.set_settings(view_settings(&snapshot.config));
            }
            cache.pending_save = None;
        }
    }
    cache.revision = Some(snapshot.revision);
    cache.previous_enabled = snapshot.config.enabled;
    window.set_actual_paused(!snapshot.config.enabled);
    if let Some(tray) = tray {
        tray.set_paused(!snapshot.config.enabled);
    }
    window.set_busy(snapshot.busy);
    window.set_twitch_status(snapshot.twitch_status.clone().into());
    window.set_auth_status(snapshot.auth.status.clone().into());
    window.set_auth_login(snapshot.auth.login.clone().into());
    window.set_auth_code(snapshot.auth.user_code.clone().into());
    window.set_auth_uri(snapshot.auth.verification_uri.clone().into());
    window.set_auth_pending(snapshot.auth.pending);
    window.set_auth_connected(snapshot.auth.connected);
    window.set_auth_persistent(snapshot.auth.persistent);
    window.set_preview_result(snapshot.preview.clone().into());
    if cache.notice != snapshot.notice {
        cache.notice = snapshot.notice.clone();
        window.set_notice(snapshot.notice.clone().into());
    }
    let provider = Provider::ALL
        .get(window.get_settings().provider as usize)
        .copied()
        .unwrap_or(Provider::Openai);
    let key = &snapshot.keys[provider.id()];
    let key_status = if key["configured"].as_bool().unwrap_or(false) {
        format!(
            "Configured · {}",
            match key["source"].as_str().unwrap_or("") {
                "file" => "saved on this computer",
                "environment" => "environment",
                _ => "this session",
            }
        )
    } else {
        "Not configured".into()
    };
    window.set_key_status(key_status.into());
    if window.get_page() != 5 || !window.window().is_visible() {
        return;
    }
    if !force_logs && !window.get_log_follow() {
        return;
    }
    let severity = match window.get_log_level() {
        1 => "info",
        2 => "warning",
        3 => "error",
        _ => "",
    };
    let subsystem = window.get_log_subsystem().to_lowercase();
    let search = window.get_log_search().to_lowercase();
    let last = snapshot
        .logs
        .last()
        .map(|entry| entry.sequence)
        .unwrap_or(0);
    let fingerprint = format!(
        "{last}:{}:{}:{severity}:{subsystem}:{search}",
        snapshot.logs.len(),
        snapshot.log_dropped
    );
    if !force_logs && cache.log_fingerprint == fingerprint {
        return;
    }
    cache.log_fingerprint = fingerprint;
    let mut text = String::new();
    let mut shown = 0;
    for entry in &snapshot.logs {
        let level = entry.level.to_lowercase();
        if !severity.is_empty() && level != severity && !(severity == "warning" && level == "warn")
        {
            continue;
        }
        if !entry.subsystem.to_lowercase().contains(&subsystem) {
            continue;
        }
        if !search.is_empty()
            && !format!("{} {} {}", entry.message, entry.details, entry.subsystem)
                .to_lowercase()
                .contains(&search)
        {
            continue;
        }
        use std::fmt::Write;
        let _ = writeln!(
            text,
            "{}  {:<7} {:<12} {}{}{}",
            entry.timestamp,
            entry.level,
            entry.subsystem,
            entry.message,
            if entry.details.is_empty() { "" } else { " · " },
            entry.details
        );
        shown += 1;
    }
    window.set_log_text(text.into());
    window.set_log_summary(
        format!(
            "{shown} shown · {} session events · {} dropped",
            snapshot.logs.len(),
            snapshot.log_dropped
        )
        .into(),
    );
    if window.get_log_follow() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(window) = weak.upgrade() {
                window.invoke_scroll_logs();
            }
        });
    }
}

pub fn run(handle: DesktopHandle) -> Result<(), Box<dyn std::error::Error>> {
    run_window(handle, None)
}

/// Called only by the explicit main --desktop-app-smoke branch with a disposable demo runtime.
pub fn run_demo_smoke(
    handle: DesktopHandle,
    directory: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    run_window(handle, Some(directory))
}

fn run_window(
    handle: DesktopHandle,
    smoke_directory: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    initialize_backend()?;
    let window = DesktopWindow::new()?;
    install_memory_estimate(&window);
    let tray = install_tray(&window);
    let cache = Arc::new(Mutex::new(ViewCache::default()));
    if let (Ok(snapshot), Ok(mut cache)) = (handle.snapshot.lock(), cache.lock()) {
        render(&window, tray.as_ref(), &snapshot, &mut cache, true);
    }
    let queued = Arc::new(AtomicBool::new(false));
    {
        let weak = window.as_weak();
        let tray = tray.as_ref().map(|tray| tray.as_weak());
        let snapshot = handle.snapshot.clone();
        let cache = cache.clone();
        let queued = queued.clone();
        *handle
            .wake
            .lock()
            .map_err(|_| "UI notification lock failed")? = Some(Box::new(move || {
            if queued.swap(true, Ordering::AcqRel) {
                return;
            }
            let weak = weak.clone();
            let tray = tray.clone();
            let snapshot = snapshot.clone();
            let cache = cache.clone();
            let queued = queued.clone();
            let queued_on_error = queued.clone();
            if slint::invoke_from_event_loop(move || {
                queued.store(false, Ordering::Release);
                if let Some(window) = weak.upgrade()
                    && let (Ok(snapshot), Ok(mut cache)) = (snapshot.lock(), cache.lock())
                {
                    render(
                        &window,
                        tray.and_then(|t| t.upgrade()).as_ref(),
                        &snapshot,
                        &mut cache,
                        false,
                    );
                }
            })
            .is_err()
            {
                queued_on_error.store(false, Ordering::Release);
            }
        }));
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        let cache = cache.clone();
        let tray = tray.as_ref().map(|t| t.as_weak());
        window.on_refresh_view(move || {
            if let Some(window) = weak.upgrade()
                && let (Ok(snapshot), Ok(mut cache)) = (handle.snapshot.lock(), cache.lock())
            {
                render(
                    &window,
                    tray.as_ref().and_then(|t| t.upgrade()).as_ref(),
                    &snapshot,
                    &mut cache,
                    true,
                );
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        let cache = cache.clone();
        window.on_save_settings(move || {
            if let Some(window) = weak.upgrade() {
                let (config, revision) = match handle.snapshot.lock() {
                    Ok(snapshot) => (
                        settings_config(&window.get_settings(), &snapshot.config),
                        snapshot.revision,
                    ),
                    Err(_) => return,
                };
                if let Err(error) = config.validate() {
                    window.set_notice(error.into());
                    return;
                }
                if submit(
                    &window,
                    &handle,
                    AppCommand::SaveConfig(Box::new(config.clone())),
                ) {
                    if let Ok(mut cache) = cache.lock() {
                        cache.pending_save = Some(config);
                        cache.pending_edit_revision = window.get_edit_revision();
                        cache.pending_config_revision = revision;
                        cache.notice.clear();
                    }
                    window.set_notice("Saving settings…".into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        let cache = cache.clone();
        window.on_save_key(move |remove| {
            if let Some(window) = weak.upgrade() {
                let id = Provider::ALL
                    .get(window.get_settings().provider as usize)
                    .copied()
                    .unwrap_or(Provider::Openai)
                    .id()
                    .to_owned();
                if submit(
                    &window,
                    &handle,
                    AppCommand::SaveKey {
                        id,
                        key: window.get_key_value().to_string(),
                        remove,
                    },
                ) {
                    if let Ok(mut cache) = cache.lock() {
                        cache.notice.clear();
                    }
                    window.set_key_value("".into());
                    window.set_notice(
                        if remove {
                            "Removing key…"
                        } else {
                            "Saving key…"
                        }
                        .into(),
                    );
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        let cache = cache.clone();
        window.on_preview(move || {
            if let Some(window) = weak.upgrade() {
                if window.get_dirty() {
                    window.set_notice("Save your settings before testing.".into());
                    return;
                }
                if submit(
                    &window,
                    &handle,
                    AppCommand::Preview(window.get_preview_input().to_string()),
                ) {
                    if let Ok(mut cache) = cache.lock() {
                        cache.notice.clear();
                    }
                    window.set_notice("Generating a private reply…".into());
                }
            }
        });
    }
    macro_rules! command_callback {
        ($method:ident, $command:expr) => {{
            let weak = window.as_weak();
            let handle = handle.clone();
            let cache = cache.clone();
            window.$method(move || {
                if let Some(window) = weak.upgrade()
                    && submit(&window, &handle, $command)
                    && let Ok(mut cache) = cache.lock()
                {
                    cache.notice.clear();
                }
            });
        }};
    }
    command_callback!(on_connect_twitch, AppCommand::ConnectTwitch);
    command_callback!(on_cancel_twitch, AppCommand::CancelTwitch);
    command_callback!(on_disconnect_twitch, AppCommand::DisconnectTwitch);
    command_callback!(on_clear_memory, AppCommand::ClearMemory);
    command_callback!(on_clear_logs, AppCommand::ClearLogs);
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        window.on_pause_bot(move || {
            if let Some(window) = weak.upgrade() {
                submit(
                    &window,
                    &handle,
                    AppCommand::Pause(!window.get_actual_paused()),
                );
            }
        });
    }
    if let Some(tray) = &tray {
        let weak = window.as_weak();
        let handle = handle.clone();
        tray.on_toggle_pause(move || {
            if let Some(window) = weak.upgrade() {
                submit(
                    &window,
                    &handle,
                    AppCommand::Pause(!window.get_actual_paused()),
                );
            }
        });
        let weak = window.as_weak();
        tray.on_open_window(move || {
            if let Some(window) = weak.upgrade() {
                restore(&window);
                window.invoke_refresh_view();
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        window.on_export_logs(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            if window.get_exporting() {
                return;
            }
            window.set_exporting(true);
            let weak = weak.clone();
            let handle = handle.clone();
            std::thread::spawn(move || {
                let selected = rfd::FileDialog::new()
                    .set_title("Export sanitized MPD Bot logs")
                    .set_file_name("mpd-bot-session.log")
                    .add_filter("Log files", &["log", "txt"])
                    .save_file();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        window.set_exporting(false);
                        if let Some(path) = selected {
                            submit(&window, &handle, AppCommand::ExportLogs(path));
                        }
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        window.on_quit(move || {
            if let Some(window) = weak.upgrade() {
                if window.get_dirty() || !window.get_key_value().is_empty() {
                    restore(&window);
                    window.set_quit_confirm(true);
                } else {
                    window.invoke_quit_now();
                }
            }
        });
        let weak = window.as_weak();
        let handle = handle.clone();
        window.on_quit_now(move || {
            if let Some(window) = weak.upgrade() {
                match handle.commands.try_send(AppCommand::Quit) {
                    Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        let _ = slint::quit_event_loop();
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        restore(&window);
                        window.set_notice(
                            "The app is applying another action. Try Quit again shortly.".into(),
                        );
                    }
                }
            }
        });
    }
    window.show()?;
    // Re-read after installing wake callbacks so a startup update cannot fall into the registration gap.
    window.invoke_refresh_view();
    let app_smoke =
        smoke_directory.map(|directory| smoke::install_app(&window, &handle, directory));
    slint::run_event_loop()?;
    if let Ok(mut wake) = handle.wake.lock() {
        *wake = None;
    }
    if let Ok(mut wake) = TRAY_FAILURE_WAKE.lock() {
        *wake = None;
    }
    if let Some(app_smoke) = app_smoke {
        app_smoke.check()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_form_roundtrip_preserves_existing_settings() {
        let config = Config {
            provider: Provider::Anthropic,
            twitch_username: "bot_account".into(),
            command_enabled: true,
            respond_to_mentions: false,
            random_reply_percent: 37,
            memory_turns: 9,
            model: "claude-opus-4-8".into(),
            personality: "Warm and welcoming — 日本語".into(),
            ..Config::default()
        };
        assert!(same_config(
            &config,
            &settings_config(&view_settings(&config), &config)
        ));
    }
    #[test]
    fn form_save_preserves_runtime_identity_and_schema() {
        let config = Config {
            twitch_username: "new_bot".into(),
            ..Config::default()
        };
        let mut view = view_settings(&config);
        view.twitch_username = "stale_bot".into();
        view.twitch_channel = "  My_Channel  ".into();
        view.excluded_users = " NightBot,\nStreamLabs, ".into();
        let saved = settings_config(&view, &config);
        assert_eq!(saved.twitch_username, "new_bot");
        assert_eq!(saved.schema_version, config.schema_version);
        assert_eq!(saved.twitch_channel, "my_channel");
        assert_eq!(saved.excluded_users, ["nightbot", "streamlabs"]);
    }
}
