//! Multi LLM server - OpenAI-compatible multi-provider proxy with a web UI.
//!
//! Runs as a single multi-user server (Docker). One port serves the web
//! management UI (`/`), the management API (`/api/*`) and the OpenAI
//! compatible inference API (`/v1/*`) for every account.

mod models_fetch;
mod oauth;
mod quota;
mod proxy;
mod secrets;
mod server;
mod settings;
mod tools;
mod users;

use std::path::PathBuf;

/// Resolve the data folder (MULTI_LLM_DATA_DIR, default /data in Docker) and
/// create it. Fails hard: a server without persistent storage would silently
/// lose every account on restart.
fn prepare_data_dir() -> PathBuf {
    let dir = settings::resolve_data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create data dir {}: {e}", dir.display());
        std::process::exit(1);
    }
    dir
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("panic: {info}");
        default(info);
    }));
}

fn main() {
    install_panic_hook();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("multillm-server {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // Admin-Passwort zurücksetzen (Notfall, im Container):
    //   docker exec -it multillm multillm-server --reset-admin
    if args.iter().any(|a| a == "--reset-admin") {
        let dir = prepare_data_dir();
        users::init(dir);
        match users::reset_admin_password() {
            Ok(()) => println!("Admin password reset to 'admin' (must be changed at next login)."),
            Err(e) => {
                eprintln!("reset failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let dir = prepare_data_dir();
    server::serve(dir);
}
