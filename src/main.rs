mod account;
mod audio;
mod credential;
mod model;
mod recovery;
mod runtime;
mod transport;
mod ui;
fn main() -> anyhow::Result<()> {
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
