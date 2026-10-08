#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account;
mod audio;
mod calls;
mod credential;
mod messaging;
mod microphone;
mod model;
mod profiles;
mod qr_login;
mod recovery;
mod runtime;
mod social;
mod transport;
mod ui;
fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("fastdistord {}", fastdistord::RELEASE_VERSION);
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--build-info") {
        println!(
            "fastdistord {} · build {}",
            fastdistord::RELEASE_VERSION,
            option_env!("FASTDISTORD_BUILD_COMMIT").unwrap_or("development")
        );
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--microphone-status") {
        println!("{}", microphone::status().label());
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--login-storage-status") {
        println!("{}", credential::storage_status());
        return Ok(());
    }
    let state = std::sync::Arc::new(std::sync::Mutex::new(model::UiState::default()));
    let gate = std::sync::Arc::new(audio::TxGate::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = runtime::spawn(state.clone(), rx, gate.clone());
    #[cfg(target_os = "macos")]
    if credential::should_restore(credential::preference(), credential::presence()) {
        // Explicit prior Remember me consent. One attempt per launch, no voice join.
        let _ = tx.send(model::Command::ConnectSaved {
            risk_accepted: true,
        });
    }
    let result = ui::run(state, tx.clone(), gate);
    let _ = tx.send(model::Command::Quit);
    drop(tx);
    let _ = worker.join();
    result
}
