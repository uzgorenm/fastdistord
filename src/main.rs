mod account;
mod audio;
mod credential;
mod messaging;
mod model;
mod recovery;
mod runtime;
mod transport;
mod ui;
fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--check-native-video") {
        #[cfg(target_os = "macos")]
        {
            fastdistord::media::macos_codec::synthetic_roundtrip(320, 180)?;
            println!(
                "Native synthetic H.264 encode/decode passed (320 x 180). No camera or screen captured."
            );
            return Ok(());
        }
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("Native video codec checks require macOS.");
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
