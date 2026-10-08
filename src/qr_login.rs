//! Unofficial native remote-auth v2. Started only by the user's Connect action.
//! Protocol reference: https://docs.discord.food/remote-authentication/desktop
//! No retry, logging, credential extraction, embedded browser or challenge bypass.
use anyhow::{Result, bail, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use futures_util::{SinkExt, StreamExt};
use rsa::{Oaep, RsaPrivateKey, RsaPublicKey, pkcs8::EncodePublicKey, rand_core::OsRng};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use zeroize::Zeroizing;

// Intentionally no Debug: these events can contain a live session credential.
pub enum Event {
    Code(qrcode::QrCode, Instant),
    AwaitingApproval,
    Token(Zeroizing<String>),
    Failed(&'static str),
}
pub struct Login {
    pub events: Receiver<Event>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for Login {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}
impl Login {
    pub fn start(ctx: egui::Context) -> Result<Self> {
        let (tx, events) = mpsc::sync_channel(4);
        let (cancel, canceled) = tokio::sync::oneshot::channel();
        std::thread::Builder::new().name("qr-login".into()).spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                notify(&tx, &ctx, Event::Failed("Could not start login. Choose Connect to try again."));
                return;
            };
            rt.block_on(async {
                tokio::select! {
                    biased;
                    _ = canceled => {},
                    result = tokio::time::timeout(Duration::from_secs(180), authenticate(&tx, &ctx)) => {
                        if !matches!(result, Ok(Ok(()))) {
                            notify(&tx, &ctx, Event::Failed("Login expired, was canceled or could not complete. Choose Connect for a fresh code. Complete any Discord security challenge in Discord."));
                        }
                    }
                }
            });
        })?;
        Ok(Self {
            events,
            cancel: Some(cancel),
        })
    }
}
fn notify(tx: &SyncSender<Event>, ctx: &egui::Context, event: Event) {
    let _ = tx.try_send(event);
    ctx.request_repaint();
}
#[derive(Deserialize)]
struct Packet {
    op: String,
    heartbeat_interval: Option<u64>,
    timeout_ms: Option<u64>,
    encrypted_nonce: Option<Zeroizing<String>>,
    fingerprint: Option<String>,
    ticket: Option<Zeroizing<String>>,
}
fn decrypt(key: &RsaPrivateKey, encoded: &str) -> Result<Zeroizing<Vec<u8>>> {
    let bytes = STANDARD.decode(encoded)?;
    Ok(Zeroizing::new(key.decrypt_blinded(
        &mut OsRng,
        Oaep::new::<Sha256>(),
        &bytes,
    )?))
}
fn fingerprint_matches(public_der: &[u8], fingerprint: &str) -> bool {
    URL_SAFE_NO_PAD.encode(Sha256::digest(public_der)) == fingerprint
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Hello,
    Nonce,
    Fingerprint,
    Scan,
    Approval,
}
impl Stage {
    fn accept(&mut self, op: &str) -> Result<()> {
        *self = match (*self, op) {
            (Self::Hello, "hello") => Self::Nonce,
            (Self::Nonce, "nonce_proof") => Self::Fingerprint,
            (Self::Fingerprint, "pending_remote_init") => Self::Scan,
            (Self::Scan, "pending_ticket") => Self::Approval,
            (Self::Approval, "pending_login") => return Ok(()),
            (_, "heartbeat_ack") => return Ok(()),
            _ => bail!("Unexpected remote-auth transition"),
        };
        Ok(())
    }
}
async fn authenticate(tx: &SyncSender<Event>, ctx: &egui::Context) -> Result<()> {
    // The documented native protocol requires this web Origin value. It is not
    // an OAuth grant or a claim of being an official Discord client.
    let mut request = "wss://remote-auth-gateway.discord.gg/?v=2".into_client_request()?;
    request
        .headers_mut()
        .insert("Origin", "https://discord.com".parse()?);
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(16 * 1024))
        .max_frame_size(Some(16 * 1024));
    let (mut socket, _) =
        tokio_tungstenite::connect_async_with_config(request, Some(config), false).await?;
    let key = RsaPrivateKey::new(&mut OsRng, 2048)?; // rsa zeroizes private components on Drop.
    let der = RsaPublicKey::from(&key).to_public_key_der()?;
    let mut deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(60));
    heartbeat.tick().await;
    let mut hello = false;
    let mut nonce_verified = false;
    let mut code_ready = false;
    let mut scanned = false;
    let mut awaiting_ack = false;
    let mut stage = Stage::Hello;
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => bail!("Expired"),
            _ = heartbeat.tick(), if hello => {
                ensure!(!awaiting_ack, "Heartbeat failed");
                socket.send(Message::Text("{\"op\":\"heartbeat\"}".into())).await?;
                awaiting_ack = true;
            },
            frame = socket.next() => {
                let frame = frame.ok_or_else(|| anyhow::anyhow!("Disconnected"))??;
                let packet: Packet = match frame {
                    Message::Text(text) => serde_json::from_str(&text)?,
                    Message::Ping(bytes) => { socket.send(Message::Pong(bytes)).await?; continue; },
                    Message::Pong(_) => continue,
                    _ => bail!("Disconnected"),
                };
                if packet.op == "cancel" { bail!("Canceled"); }
                stage.accept(&packet.op)?;
                match packet.op.as_str() {
                    "hello" => {
                        ensure!(!hello, "Repeated hello");
                        let interval = packet.heartbeat_interval.filter(|n| (1000..=120000).contains(n)).ok_or_else(|| anyhow::anyhow!("Invalid heartbeat"))?;
                        let timeout = packet.timeout_ms.filter(|n| (1000..=180000).contains(n)).ok_or_else(|| anyhow::anyhow!("Invalid expiry"))?;
                        deadline = tokio::time::Instant::now() + Duration::from_millis(timeout);
                        heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_millis(interval), Duration::from_millis(interval));
                        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        socket.send(Message::Text(serde_json::json!({"op":"init","encoded_public_key":STANDARD.encode(der.as_bytes())}).to_string().into())).await?;
                        hello = true;
                    },
                    "nonce_proof" => {
                        ensure!(hello && !nonce_verified, "Unexpected nonce");
                        let nonce = decrypt(&key, packet.encrypted_nonce.as_deref().map(|n| n.as_str()).ok_or_else(|| anyhow::anyhow!("Missing nonce"))?)?;
                        let proof = Zeroizing::new(URL_SAFE_NO_PAD.encode(nonce.as_slice()));
                        socket.send(Message::Text(format!("{{\"op\":\"nonce_proof\",\"nonce\":\"{}\"}}", proof.as_str()).into())).await?;
                        nonce_verified = true;
                    },
                    "pending_remote_init" => {
                        ensure!(nonce_verified && !code_ready, "Unexpected fingerprint");
                        let fingerprint = packet.fingerprint.ok_or_else(|| anyhow::anyhow!("Missing fingerprint"))?;
                        ensure!(fingerprint_matches(der.as_bytes(), &fingerprint), "Fingerprint mismatch");
                        let code = qrcode::QrCode::new(format!("https://discord.com/ra/{fingerprint}"))?;
                        notify(tx, ctx, Event::Code(code, Instant::now() + deadline.saturating_duration_since(tokio::time::Instant::now())));
                        code_ready = true;
                    },
                    "pending_ticket" => {
                        ensure!(code_ready && !scanned, "Unexpected scan");
                        // Do not deserialize/decrypt/store the unsolicited account profile.
                        notify(tx, ctx, Event::AwaitingApproval);
                        scanned = true;
                    },
                    "pending_login" => {
                        ensure!(scanned, "Approval before scan");
                        let ticket = packet.ticket.ok_or_else(|| anyhow::anyhow!("Missing ticket"))?;
                        let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).redirect(reqwest::redirect::Policy::none()).build()?;
                        // Own the serialized ticket in an erased buffer; transport
                        // libraries may also hold temporary request buffers.
                        let payload = Zeroizing::new(serde_json::to_vec(&serde_json::json!({"ticket":ticket.as_str()}))?);
                        let mut response = http.post("https://discord.com/api/v9/users/@me/remote-auth/login")
                            .header(reqwest::header::CONTENT_TYPE, "application/json")
                            .body(payload.to_vec()).send().await?;
                        ensure!(response.status().is_success(), "Login rejected; no challenge or rate-limit retry");
                        #[derive(Deserialize)]
                        struct TokenResponse { encrypted_token: Zeroizing<String> }
                        let mut buffer = Zeroizing::new(Vec::new());
                        while let Some(chunk) = response.chunk().await? {
                            ensure!(buffer.len() + chunk.len() <= 16 * 1024, "Oversized response");
                            buffer.extend_from_slice(&chunk);
                        }
                        let body: TokenResponse = serde_json::from_slice(&buffer)?;
                        let bytes = decrypt(&key, &body.encrypted_token)?;
                        let token = Zeroizing::new(String::from_utf8(bytes.to_vec())?);
                        ensure!(!token.is_empty() && token.len() < 4096 && !token.contains(['\r','\n']), "Invalid credential");
                        let _ = socket.close(None).await;
                        notify(tx, ctx, Event::Token(token));
                        return Ok(());
                    },
                    "heartbeat_ack" => { ensure!(awaiting_ack, "Unexpected heartbeat ack"); awaiting_ack = false; },
                    "cancel" => bail!("Canceled"),
                    _ => bail!("Unknown protocol event"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qr_fingerprint_must_match_our_ephemeral_public_key() {
        let public = b"offline fixture, not a Discord session";
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(public));
        assert!(fingerprint_matches(public, &expected));
        assert!(!fingerprint_matches(b"different key", &expected));
        assert!(!fingerprint_matches(public, "arbitrary-link"));
    }
    #[test]
    fn encrypted_payload_round_trip_uses_oaep_sha256() {
        let private = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let public = RsaPublicKey::from(&private);
        let encoded = STANDARD.encode(
            public
                .encrypt(
                    &mut OsRng,
                    Oaep::new::<Sha256>(),
                    b"offline-test-not-a-token",
                )
                .unwrap(),
        );
        assert_eq!(
            decrypt(&private, &encoded).unwrap().as_slice(),
            b"offline-test-not-a-token"
        );
        assert!(decrypt(&private, "not-base64").is_err());
    }
    #[test]
    fn offline_mock_sequence_rejects_early_approval_and_repeated_handshake() {
        // Protocol fixture only: never opens a socket or creates a QR session.
        let mut stage = Stage::Hello;
        assert!(stage.accept("pending_login").is_err());
        assert!(stage.accept("hello").is_ok());
        assert!(stage.accept("hello").is_err());
        assert!(stage.accept("nonce_proof").is_ok());
        assert!(stage.accept("pending_ticket").is_err());
        for op in ["pending_remote_init", "pending_ticket", "pending_login"] {
            assert!(stage.accept(op).is_ok());
        }
        assert!(stage.accept("unexpected").is_err());
    }
}
