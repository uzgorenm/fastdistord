#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account;
mod audio;
mod credential;
mod messaging;
mod model;
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
    let state = std::sync::Arc::new(std::sync::Mutex::new(model::UiState::default()));
    let gate = std::sync::Arc::new(audio::TxGate::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = runtime::spawn(state.clone(), rx, gate.clone());
    let result = ui::run(state, tx.clone(), gate);
    let _ = tx.send(model::Command::Quit);
    drop(tx);
    let _ = worker.join();
    result
}
