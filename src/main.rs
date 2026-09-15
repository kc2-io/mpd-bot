#![forbid(unsafe_code)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod application;
mod config;
mod credential_files;
mod diagnostics;
mod engine;
mod provider;
mod secrets;
mod twitch;
mod ui;

use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

fn main() {
    if let Err(error) = launch() {
        if std::env::args().any(|arg| arg == "--desktop-app-smoke") {
            eprintln!("Desktop app smoke failed: {error}");
            std::process::exit(1);
        }
        // Only curated startup errors reach this dialog. No credentials or panic payloads.
        rfd::MessageDialog::new()
            .set_title("MPD Bot")
            .set_description(&error)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}
fn default_directory() -> Result<PathBuf, String> {
    directories::ProjectDirs::from("io", "kc2", "mpd-bot")
        .map(|d| d.config_dir().to_path_buf())
        .ok_or_else(|| "Could not locate your settings directory.".into())
}
fn instance_lock(directory: &Path) -> Result<File, String> {
    std::fs::create_dir_all(directory).map_err(|_| "Could not create the settings directory.")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("instance.lock"))
        .map_err(|_| "Could not open the application lock.")?;
    lock.try_lock_exclusive()
        .map_err(|_| "MPD Bot is already running. Open it from the Windows system tray.")?;
    Ok(lock)
}
fn launch() -> Result<(), String> {
    let mut directory = None;
    let mut demo = false;
    let mut spike = false;
    let mut app_smoke = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                directory = Some(PathBuf::from(
                    args.next().ok_or("--data-dir requires a path")?,
                ))
            }
            "--demo" => demo = true,
            "--desktop-spike" => spike = true,
            "--desktop-app-smoke" => app_smoke = true,
            "--help" | "-h" => {
                rfd::MessageDialog::new().set_title("MPD Bot").set_description("MPD Bot desktop\n--data-dir PATH: custom settings directory\n--demo: isolated UI without saved credentials or Twitch\nClose or minimize the window to use the tray. Choose Quit to stop.").show();
                return Ok(());
            }
            "--port" => return Err(
                "The desktop app no longer uses a browser port. Remove --port and launch again."
                    .into(),
            ),
            _ => {
                return Err(
                    "Unknown command-line option. Use --help for supported options.".into(),
                );
            }
        }
    }
    if spike {
        return ui::run_spike().map_err(|_| "The native UI experiment failed.".into());
    }
    if app_smoke {
        // This explicit test mode ignores --data-dir and never loads real settings,
        // credentials, or environment Client IDs. The disposable demo disables Twitch.
        let directory =
            tempfile::tempdir().map_err(|_| "Could not create isolated smoke directory")?;
        let (desktop, worker) =
            application::start(directory.path().to_owned(), config::Config::default(), true);
        let result =
            ui::run_demo_smoke(desktop, directory.path()).map_err(|error| error.to_string());
        worker.shutdown();
        return result;
    }
    let global = default_directory()?;
    let directory = directory.unwrap_or_else(|| {
        if demo {
            std::env::temp_dir().join("mpd-bot-demo")
        } else {
            global.clone()
        }
    });
    // Keep one real bot process per OS account, even with separate --data-dir stores.
    // Demo never loads existing credentials or starts Twitch tasks.
    let _lock = instance_lock(if demo { &directory } else { &global })?;
    std::fs::create_dir_all(&directory).map_err(|_| "Could not create settings directory.")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not secure the settings directory.")?;
    }
    if !demo
        && std::net::TcpStream::connect_timeout(
            &"127.0.0.1:9847".parse().unwrap(),
            std::time::Duration::from_millis(150),
        )
        .is_ok()
    {
        return Err("The earlier browser POC may still be running on port 9847. Close it before starting this desktop build.".into());
    }
    let config = if demo {
        config::Config::default()
    } else {
        config::load(&directory).map_err(|_|"Could not read settings. Check config.json format/version and directory permissions; your file was not overwritten.")?
    };
    let crash_path = directory.join("last-crash.txt");
    std::panic::set_hook(Box::new(move |_| {
        let _ = std::fs::write(
            &crash_path,
            "MPD Bot 0.1.0 encountered an unexpected error. No prompt, credential or panic payload was recorded.\n",
        );
    }));
    let (desktop, worker) = application::start(directory, config, demo);
    let result = ui::run(desktop)
        .map_err(|_| "Could not open the native settings window. Check display support.");
    worker.shutdown();
    Ok(result?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn instance_lock_releases_after_owner_exits() {
        let dir = tempfile::tempdir().unwrap();
        let first = instance_lock(dir.path()).unwrap();
        assert!(instance_lock(dir.path()).is_err());
        drop(first);
        assert!(instance_lock(dir.path()).is_ok());
    }
}
