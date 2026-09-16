//! Explicit, isolated native smoke exercise. Never used by normal startup.
use super::*;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    initialize_backend()?;
    let window = DesktopWindow::new()?;
    window.set_app_version(crate::VERSION.into());
    install_memory_estimate(&window);
    let fixture = Config {
        bot_name: "MPD Bot".into(),
        model: "claude-opus-4-8".into(),
        provider: Provider::Anthropic,
        twitch_channel: "development_channel".into(),
        ..Config::default()
    };
    window.set_settings(view_settings(&fixture));
    let profiles = crate::profiles::Profiles::from_legacy(
        &fixture,
        &crate::secrets::Secrets {
            values: Default::default(),
            warning: None,
        },
    );
    let profile_snapshot = AppSnapshot {
        profiles: profiles.summaries(),
        active_profile_id: profiles.active_id().into(),
        profiles_ready: true,
        ..AppSnapshot::default()
    };
    show_profile(&window, &profile_snapshot);
    window.set_profiles_ready(true);
    window.set_active_profile_name(profiles.active().name.clone().into());
    window.set_key_status("Not configured (isolated UI fixture)".into());
    window.set_auth_status("Ready to connect".into());
    window.set_twitch_status("Disconnected".into());
    window.set_preview_result(
        "Ready when you are. Bring the good vibes; I’ll bring the questionable dad jokes.".into(),
    );
    window.set_log_text("13:00:00  INFO    app       MPD Bot started\n13:00:01  INFO    settings  Settings saved\n13:00:02  WARN    provider  Request failed · HTTP 404 · request ID: example\n".into());
    window.set_log_summary("3 shown · 3 session events · 0 dropped".into());
    let tray = install_tray(&window);
    if let Some(tray) = &tray {
        let weak = tray.as_weak();
        tray.on_toggle_pause(move || {
            if let Some(tray) = weak.upgrade() {
                tray.set_paused(!tray.get_paused());
            }
        });
    }
    let weak = window.as_weak();
    window.on_quit(move || {
        if let Some(window) = weak.upgrade() {
            if window.get_dirty() || window.get_profile_dirty() {
                window.set_quit_confirm(true);
            } else {
                let _ = slint::quit_event_loop();
            }
        }
    });
    window.on_quit_now(|| {
        let _ = slint::quit_event_loop();
    });
    window.show()?;
    if let Ok(value) = std::env::var("MPD_DESKTOP_IDLE_SECONDS") {
        let seconds: u64 = value
            .parse()
            .map_err(|_| "MPD_DESKTOP_IDLE_SECONDS must be 1–3600")?;
        if !(1..=3600).contains(&seconds) {
            return Err("MPD_DESKTOP_IDLE_SECONDS must be 1–3600".into());
        }
        let weak = window.as_weak();
        slint::Timer::single_shot(Duration::from_secs(1), move || {
            let window = weak.unwrap();
            assert!(!TRAY_FAILED.load(Ordering::Acquire));
            window.invoke_hide_to_tray();
            assert!(!window.window().is_visible());
            println!("Desktop idle smoke: hidden");
        });
        slint::Timer::single_shot(Duration::from_secs(seconds + 5), || {
            println!("Desktop idle smoke: complete");
            let _ = slint::quit_event_loop();
        });
        slint::run_event_loop()?;
        if let Ok(mut wake) = TRAY_FAILURE_WAKE.lock() {
            *wake = None;
        }
        return Ok(());
    }
    if std::env::var_os("MPD_DESKTOP_SMOKE").is_some() {
        if let Some(directory) = std::env::var_os("MPD_DESKTOP_SCREENSHOTS") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory)?;
            for (page, name) in [
                "personality",
                "provider",
                "twitch",
                "chatters",
                "limits",
                "preview",
                "logs",
                "about",
            ]
            .into_iter()
            .enumerate()
            {
                let weak = window.as_weak();
                slint::Timer::single_shot(
                    Duration::from_millis(200 + page as u64 * 260),
                    move || {
                        if let Some(window) = weak.upgrade() {
                            window.set_page(page as i32);
                        }
                    },
                );
                let weak = window.as_weak();
                let path = directory.join(format!("page-{page}-{name}.png"));
                slint::Timer::single_shot(
                    Duration::from_millis(350 + page as u64 * 260),
                    move || {
                        snapshot(&weak.unwrap(), &path).expect("native smoke snapshot");
                    },
                );
            }
        }
        let weak = window.as_weak();
        slint::Timer::single_shot(Duration::from_millis(2300), move || {
            let window = weak.unwrap();
            assert!(window.window().is_visible());
            assert!(
                !TRAY_FAILED.load(Ordering::Acquire),
                "native tray creation failed"
            );
            window.window().set_minimized(true);
            println!("Desktop smoke: requested native minimize");
        });
        let weak = window.as_weak();
        let tray_weak = tray.as_ref().map(|t| t.as_weak());
        slint::Timer::single_shot(Duration::from_millis(3200), move || {
            assert!(
                !weak.unwrap().window().is_visible(),
                "native minimize must hide to tray"
            );
            let tray = tray_weak.and_then(|t| t.upgrade()).expect("native tray");
            tray.invoke_open_window();
            tray.invoke_toggle_pause();
            assert!(tray.get_paused());
            println!("Desktop smoke: native minimize hid window; tray Open and Pause handled");
        });
        let weak = window.as_weak();
        slint::Timer::single_shot(Duration::from_millis(3900), move || {
            let window = weak.unwrap();
            assert!(
                window.window().is_visible(),
                "restore after native minimize"
            );
            assert!(!window.window().is_minimized());
            window.invoke_hide_to_tray();
            assert!(!window.window().is_visible());
            restore(&window);
            assert!(window.window().is_visible());
            window.set_dirty(true);
            window.invoke_quit();
            assert!(window.get_quit_confirm(), "unsaved draft guard");
            window.set_quit_confirm(false);
            window.set_dirty(false);
            println!("Desktop smoke: restored, explicit hide, draft quit guard passed");
        });
        if std::env::var_os("MPD_DESKTOP_FORCE_TRAY_FAILURE").is_some() {
            slint::Timer::single_shot(Duration::from_millis(4500), || {
                log::debug!(target: "i_slint_core::debug_log", "Slint: Failed to create system tray icon: simulated smoke failure");
            });
            let weak = window.as_weak();
            slint::Timer::single_shot(Duration::from_millis(5200), move || {
                let window = weak.unwrap();
                assert!(TRAY_FAILED.load(Ordering::Acquire));
                assert!(window.window().is_visible());
                assert!(!window.get_tray_available());
                window.invoke_hide_to_tray();
                assert!(
                    window.window().is_visible(),
                    "tray failure must not trap user"
                );
                println!("Desktop smoke: forced tray failure restored window and disabled hiding");
            });
        }
        let tray_weak = tray.as_ref().map(|t| t.as_weak());
        slint::Timer::single_shot(Duration::from_millis(6000), move || {
            println!("Desktop smoke: Quit");
            if let Some(tray) = tray_weak.and_then(|t| t.upgrade()) {
                tray.invoke_quit();
            } else {
                let _ = slint::quit_event_loop();
            }
        });
    }
    slint::run_event_loop()?;
    if let Ok(mut wake) = TRAY_FAILURE_WAKE.lock() {
        *wake = None;
    }
    Ok(())
}

fn snapshot(window: &DesktopWindow, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let pixels = window.window().take_snapshot()?;
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        pixels.width(),
        pixels.height(),
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()?
        .write_image_data(pixels.as_bytes())?;
    Ok(())
}

/// Drives production UI callbacks against main's hardwired disposable demo runtime.
/// The only credential action is a locally persisted synthetic key; no Preview or OS credential-store operation.
pub(super) struct AppSmoke {
    _timer: slint::Timer,
    outcome: std::rc::Rc<std::cell::RefCell<Option<Result<(), String>>>>,
}
impl AppSmoke {
    pub(super) fn check(self) -> Result<(), String> {
        self.outcome
            .borrow_mut()
            .take()
            .unwrap_or_else(|| Err("Desktop app smoke closed before completion.".into()))
    }
}
pub(super) fn install_app(
    window: &DesktopWindow,
    handle: &DesktopHandle,
    directory: &Path,
) -> AppSmoke {
    const NAME: &str = "Native callback smoke";
    const KEY: &str = "synthetic-local-file-smoke-value";
    let outcome = std::rc::Rc::new(std::cell::RefCell::new(None));
    let result = outcome.clone();
    let weak = window.as_weak();
    let handle = handle.clone();
    let directory = directory.to_owned();
    let started = std::time::Instant::now();
    let mut phase = 0;
    let mut invalid_revision = 0;
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
        if result.borrow().is_some() { return; }
        let Some(window) = weak.upgrade() else {
            *result.borrow_mut() = Some(Err("Desktop app smoke window disappeared.".into()));
            let _ = slint::quit_event_loop(); return;
        };
        let step = (|| -> Result<bool, String> {
            if started.elapsed() > Duration::from_secs(45) { return Err(format!("Desktop app smoke timed out at phase {phase}.")); }
            let state = handle.snapshot.lock().map_err(|_| "Desktop snapshot lock failed")?.clone();
            match phase {
                0 => {
                    let mut settings = window.get_settings(); settings.bot_name = NAME.into();
                    settings.command_enabled = true; settings.command = "!ask".into();
                    settings.respond_to_mentions = false; settings.random_reply_percent = 37;
                    settings.remembered_messages = 0;
                    window.set_settings(settings.clone());
                    if !window.get_memory_estimate_text().contains("0 MiB") { return Err("Disabled memory estimate is incorrect".into()); }
                    settings.remembered_messages = 8; window.set_settings(settings);
                    if window.get_memory_estimate_text().contains("0 MiB") { return Err("Memory estimate did not react to settings".into()); }
                    window.invoke_edited_settings(); window.invoke_save_settings(); phase = 1;
                }
                1 if state.config.bot_name == NAME && !window.get_dirty() => {
                    let saved = crate::config::load(&directory).map_err(|_| "Desktop Save callback did not persist valid settings")?;
                    if saved.bot_name != NAME || !saved.command_enabled || saved.command != "!ask"
                        || saved.respond_to_mentions || saved.random_reply_percent != 37 || saved.memory_turns != 8 {
                        return Err("Desktop Save callback persisted wrong settings.".into());
                    }
                    println!("Desktop app smoke: Save callback persisted settings and cleared dirty state");
                    window.invoke_pause_bot(); phase = 2;
                }
                2 if !state.config.enabled && window.get_actual_paused() => {
                    println!("Desktop app smoke: Pause callback reached runtime");
                    window.set_key_value(KEY.into()); window.set_profile_dirty(true); window.invoke_save_profile(); phase = 3;
                }
                3 if !window.get_profile_busy() && state.profiles.iter().any(|p| p.profile.id == state.active_profile_id && p.has_key) => {
                    if !window.get_key_value().is_empty() { return Err("Saved key remained in native input.".into()); }
                    if state.logs.iter().any(|entry| entry.message.contains(KEY) || entry.details.contains(KEY)) {
                        return Err("Synthetic credential appeared in logs.".into());
                    }
                    println!("Desktop app smoke: locally persisted synthetic key saved, input cleared, logs sanitized");
                    invalid_revision = state.revision;
                    let mut settings = window.get_settings(); settings.bot_name = "".into();
                    window.set_settings(settings); window.invoke_edited_settings(); window.invoke_save_settings();
                    if !window.get_notice().contains("Bot name") { return Err("Invalid form was not rejected visibly.".into()); }
                    phase = 4;
                }
                4 => {
                    if state.revision != invalid_revision || state.config.bot_name != NAME {
                        return Err("Invalid form changed applied settings.".into());
                    }
                    println!("Desktop app smoke: invalid settings rejected without changing runtime");
                    let mut settings = window.get_settings(); settings.bot_name = NAME.into();
                    window.set_settings(settings); window.invoke_edited_settings(); window.invoke_save_settings(); phase = 5;
                }
                5 if state.revision > invalid_revision && !window.get_dirty() => {
                    window.invoke_connect_twitch(); phase = 6;
                }
                6 if state.notice.contains("public app registration") && window.get_notice().contains("public app registration") => {
                    if state.auth.connected || state.auth.pending { return Err("Demo smoke unexpectedly began authorization.".into()); }
                    println!("Desktop app smoke: missing Twitch registration produced UI feedback without authorization");
                    window.set_page(6); window.invoke_refresh_view();
                    if window.get_log_text().contains(KEY) { return Err("Synthetic credential appeared in native log view.".into()); }
                    window.set_page(1); window.invoke_new_profile();
                    window.set_profile_name("Second OpenAI".into()); window.set_profile_model("synthetic-model-two".into());
                    window.set_key_value("synthetic-second-profile-key".into()); window.invoke_save_profile(); phase = 7;
                }
                7 if !window.get_profile_busy() && state.profiles.len() == 2 => {
                    if window.get_profile_dirty() || !window.get_key_value().is_empty() { return Err("Profile save left a dirty draft or visible key".into()); }
                    if window.get_profile_name() != "Second OpenAI" || state.config.model != "synthetic-model-two" { return Err("New profile was not activated".into()); }
                    window.invoke_select_profile(0); phase = 8;
                }
                8 if !window.get_profile_busy() && state.active_profile_id == "imported-openai" => {
                    let no_keys = crate::secrets::Secrets { values: Default::default(), warning: None };
                    let saved = crate::profiles::Profiles::load_or_migrate(&directory, &state.config, &no_keys)?;
                    if saved.key() != KEY { return Err("Switching same-provider profiles mixed their keys".into()); }
                    window.invoke_select_profile(1); phase = 9;
                }
                9 if !window.get_profile_busy() && window.get_profile_name() == "Second OpenAI" => {
                    window.set_profile_name("Renamed profile".into()); window.set_profile_dirty(true); window.invoke_save_profile(); phase = 10;
                }
                10 if !window.get_profile_busy() && window.get_profile_name() == "Renamed profile" && !window.get_profile_dirty() => {
                    let no_keys = crate::secrets::Secrets { values: Default::default(), warning: None };
                    let saved = crate::profiles::Profiles::load_or_migrate(&directory, &state.config, &no_keys)?;
                    if saved.key() != "synthetic-second-profile-key" { return Err("Renaming profile lost its saved key".into()); }
                    window.invoke_new_profile(); window.set_profile_name("Draft only".into()); window.invoke_discard_profile();
                    if window.get_profile_dirty() || window.get_profile_name() != "Renamed profile" { return Err("Discard did not restore the active profile".into()); }
                    window.invoke_delete_profile(); phase = 11;
                }
                11 if !window.get_profile_busy() && state.profiles.len() == 1 => {
                    window.invoke_remove_profile_key(); phase = 12;
                }
                12 if !window.get_profile_busy() && !state.profiles[0].has_key => {
                    println!("Desktop app smoke: named profile create/save/switch/rename/discard/delete and key isolation passed");
                    window.set_page(3); window.invoke_new_chatter();
                    window.set_chatter_login("@SpaceRanger".into()); window.set_chatter_nickname("Ranger".into());
                    window.set_chatter_description("Enjoys retro games and Friday streams.".into());
                    window.set_chatter_sarcastic(true); window.set_chatter_praise(true); window.set_chatter_hero(true); window.set_chatter_regular(true);
                    window.set_chatter_never_respond(true); window.invoke_edited_chatter(); window.invoke_save_chatter(); phase = 13;
                }
                13 if !window.get_chatter_busy() && !window.get_chatter_dirty() && state.chatters.profile_count == 1 => {
                    let saved = crate::chatters::Profiles::load(&directory)?;
                    let profile = saved.resolve("unknown","spaceranger").ok_or("Manual chatter not persisted")?;
                    if !profile.never_respond || !profile.styles.sarcastic || !profile.styles.praise || !profile.styles.hero || !profile.styles.regular || profile.nickname != "Ranger" { return Err("Chatter style/block persistence incorrect".into()); }
                    if let Some(path) = std::env::var_os("MPD_CHATTER_SCREENSHOTS") {
                        let path = PathBuf::from(path); std::fs::create_dir_all(&path).map_err(|_| "Screenshot directory failed")?;
                        snapshot(&window,&path.join("chatters.png")).map_err(|_| "Chatter screenshot failed")?;
                    }
                    window.invoke_new_chatter(); window.set_chatter_login("secondviewer".into()); window.invoke_save_chatter(); phase = 14;
                }
                14 if !window.get_chatter_busy() && state.chatters.profile_count == 2 => {
                    if window.get_chatter_dirty() || !window.get_chatter_error().is_empty() { return Err("New chatter after selected profile failed".into()); }
                    window.set_chatter_description("x".repeat(501).into()); window.invoke_edited_chatter(); window.invoke_save_chatter(); phase = 15;
                }
                15 if !window.get_chatter_busy() && state.chatters.error.is_some() => {
                    if !window.get_chatter_dirty() { return Err("Rejected chatter draft was discarded".into()); }
                    window.invoke_quit(); if !window.get_quit_confirm() { return Err("Chatter draft did not guard Quit".into()); }
                    window.set_quit_confirm(false); window.invoke_discard_chatter();
                    if window.get_chatter_dirty() || !window.get_chatter_description().is_empty() { return Err("Chatter discard failed".into()); }
                    window.invoke_clear_seen_chatters(); phase = 16;
                }
                16 if !window.get_chatter_busy() && state.chatters.error.is_none() => {
                    if state.chatters.profile_count != 2 { return Err("Clear seen deleted curated profiles".into()); }
                    window.invoke_delete_chatter(); phase = 17;
                }
                17 if !window.get_chatter_busy() && state.chatters.profile_count == 1 => {
                    let saved = crate::chatters::Profiles::load(&directory)?;
                    if !saved.resolve("unknown","spaceranger").is_some_and(|p|p.never_respond) { return Err("Deleting another chatter lost saved deny".into()); }
                    if state.logs.iter().any(|e|e.message.contains("SpaceRanger") || e.details.contains("Ranger")) { return Err("Profile details leaked into logs".into()); }
                    println!("Desktop app smoke: chatter create, four styles, deny, restart storage, second draft, validation, discard, Quit, clear seen and delete passed");
                    return Ok(true);
                }
                _ => {}
            }
            Ok(false)
        })();
        match step {
            Ok(false) => {}
            Ok(true) => {
                *result.borrow_mut() = Some(Ok(()));
                println!("Desktop app smoke: PASS; invoking production Quit callback");
                window.invoke_quit();
            }
            Err(error) => {
                eprintln!("{error}"); *result.borrow_mut() = Some(Err(error));
                window.set_dirty(false); window.set_key_value("".into()); window.invoke_quit_now();
            }
        }
    });
    AppSmoke {
        _timer: timer,
        outcome,
    }
}
