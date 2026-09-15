#![forbid(unsafe_code)]
mod config;
mod engine;
mod provider;
mod secrets;
mod server;
mod twitch;

use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, RwLock, watch};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut directory = None;
    let mut port: u16 = 9847;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => directory = Some(PathBuf::from(args.next().ok_or("--data-dir needs a path")?)),
            "--port" => { port = args.next().ok_or("--port needs a number")?.parse()?; if port == 0 { return Err("Port must be nonzero".into()); } },
            "--help" | "-h" => { println!("mpd-bot [--port 9847] [--data-dir PATH]\nOpen the printed configuration link. Ctrl+C stops the bot."); return Ok(()); },
            _ => return Err(format!("Unknown option: {arg}").into()),
        }
    }
    let directory = directory.or_else(|| directories::ProjectDirs::from("io", "kc2", "mpd-bot").map(|d|d.config_dir().to_path_buf())).ok_or("Could not find your configuration directory")?;
    std::fs::create_dir_all(&directory)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory,std::fs::Permissions::from_mode(0o700))?;
    }
    // Bind before reading credentials so a second instance cannot run another bot.
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let config = config::load(&directory)?;
    let secrets = tokio::task::spawn_blocking(secrets::Secrets::load).await?;
    let mut random = [0u8;32];
    getrandom::fill(&mut random).map_err(|_| "Could not generate a secure local session token")?;
    let token: String = random.iter().map(|b|format!("{b:02x}")).collect();
    let origin = format!("http://127.0.0.1:{port}");
    println!("MPD Bot\nConfigure: {origin}/#{token}\nSettings: {}\nClose the browser when done; keep this process running. Ctrl+C to stop.", directory.display());
    let (changed, receiver) = watch::channel(0);
    let app = Arc::new(server::App { config:RwLock::new(config), secrets:RwLock::new(secrets), engine:Mutex::new(engine::Engine::default()), client:provider::client()?, admin_token:token, origin, directory, changed, twitch_status:RwLock::new("Not connected".into()) });
    let twitch_task = tokio::spawn(twitch::run(app.clone(), receiver));
    axum::serve(listener, server::router(app)).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    twitch_task.abort();
    Ok(())
}
