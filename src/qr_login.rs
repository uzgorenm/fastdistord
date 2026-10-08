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

/// Only locally defined categories and numeric status codes cross into the UI.
/// Never display server bodies, headers, close reasons, tickets or payloads.
#[derive(Clone, Copy, Debug)]
enum Failure {
    Network,
    Tls,
    Http(u16),
    HelloTimeout,
    Expired,
    Canceled,
    Disconnected,
    Closed(u16),
    Heartbeat,
    Protocol,
    Crypto,
    Fingerprint,
    ExchangeNetwork,
    ExchangeHttp(u16),
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}
impl std::error::Error for Failure {}
impl Failure {
    fn message(self) -> String {
        match self {
            Self::Network => "Could not reach Discord’s login service. Check your connection and choose Connect to try again.".into(),
            Self::Tls => "The secure connection to Discord’s login service failed. No certificate checks were bypassed.".into(),
            Self::Http(429) | Self::ExchangeHttp(429) => "Discord rate-limited this login attempt. Wait before choosing Connect again.".into(),
            Self::Http(code) => format!("Discord rejected the login connection (HTTP {code}). No QR code was created."),
            Self::HelloTimeout => "Discord connected but did not start the login handshake. Choose Connect to try again.".into(),
            Self::Expired | Self::Closed(4003) => "The login code expired. Choose Connect for a fresh code.".into(),
            Self::Canceled => "Login was canceled on your phone. Choose Connect to start again.".into(),
            Self::Disconnected | Self::Closed(1000) => "Discord closed the login connection before it completed. Choose Connect to start again.".into(),
            Self::Closed(code) => format!("Discord closed the login handshake (code {code}). This client may not support the current protocol."),
            Self::Heartbeat => "Discord’s login connection stopped responding. Choose Connect to start again.".into(),
            Self::Protocol => "Discord sent a login response this version could not handle. No account connection was made.".into(),
            Self::Crypto => "Could not verify Discord’s encrypted login response. Login stopped safely.".into(),
            Self::Fingerprint => "Discord’s QR fingerprint did not match this login’s key. Login stopped safely.".into(),
            Self::ExchangeNetwork => "Your phone approved login, but the session exchange could not reach Discord. Choose Connect to start again.".into(),
            Self::ExchangeHttp(code) => format!("Your phone approved login, but Discord rejected the session exchange (HTTP {code}). No challenge or rate-limit bypass was attempted."),
        }
    }
}
fn connection_failure(error: tokio_tungstenite::tungstenite::Error) -> Failure {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Http(response) => Failure::Http(response.status().as_u16()),
        Error::Io(_) => Failure::Network,
        Error::Tls(_) => Failure::Tls,
        Error::ConnectionClosed | Error::AlreadyClosed => Failure::Disconnected,
        _ => Failure::Protocol,
    }
}
const LOCAL_LOGIN_LIMIT_MS: u64 = 180_000;
fn negotiated_timing(heartbeat: Option<u64>, timeout: Option<u64>) -> Result<(Duration, Duration)> {
    let heartbeat = heartbeat
        .filter(|n| (1000..=120_000).contains(n))
        .ok_or(Failure::Protocol)?;
    // Server lifetimes can exceed our local limit (observed 308377 ms).
    // Cap the local attempt instead of treating a longer lifetime as invalid.
    let timeout = timeout.filter(|n| *n >= 1000).ok_or(Failure::Protocol)?;
    Ok((
        Duration::from_millis(heartbeat),
        Duration::from_millis(timeout.min(LOCAL_LOGIN_LIMIT_MS)),
    ))
}

// Intentionally no Debug: these events can contain a live session credential.
pub enum Event {
    Code(qrcode::QrCode, Instant),
    AwaitingApproval,
    Token(Zeroizing<String>),
    Failed(String),
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
                notify(&tx, &ctx, Event::Failed("Could not start login. Choose Connect to try again.".into()));
                return;
            };
            rt.block_on(async {
                tokio::select! {
                    biased;
                    _ = canceled => {},
                    result = tokio::time::timeout(Duration::from_secs(180), authenticate(&tx, &ctx)) => {
                        let failure = match result {
                            Ok(Ok(())) => None,
                            Ok(Err(error)) => Some(error.downcast_ref::<Failure>().copied().unwrap_or(Failure::Protocol)),
                            Err(_) => Some(Failure::Expired),
                        };
                        if let Some(failure) = failure { notify(&tx, &ctx, Event::Failed(failure.message())); }
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
            _ => bail!(Failure::Protocol),
        };
        Ok(())
    }
}
async fn connect_gateway() -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    // The documented native protocol requires this web Origin value. It is not
    // an OAuth grant or a claim of being an official Discord client.
    let mut request = "wss://remote-auth-gateway.discord.gg/?v=2".into_client_request()?;
    request
        .headers_mut()
        .insert("Origin", "https://discord.com".parse()?);
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(16 * 1024))
        .max_frame_size(Some(16 * 1024));
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| Failure::Network)?
    .map_err(connection_failure)?;
    Ok(socket)
}
async fn authenticate(tx: &SyncSender<Event>, ctx: &egui::Context) -> Result<()> {
    let mut socket = connect_gateway().await?;
    let key = RsaPrivateKey::new(&mut OsRng, 2048).map_err(|_| Failure::Crypto)?; // rsa zeroizes private components on Drop.
    let der = RsaPublicKey::from(&key)
        .to_public_key_der()
        .map_err(|_| Failure::Crypto)?;
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
            _ = tokio::time::sleep_until(deadline) => bail!(if hello { Failure::Expired } else { Failure::HelloTimeout }),
            _ = heartbeat.tick(), if hello => {
                ensure!(!awaiting_ack, Failure::Heartbeat);
                socket.send(Message::Text("{\"op\":\"heartbeat\"}".into())).await?;
                awaiting_ack = true;
            },
            frame = socket.next() => {
                let frame = frame.ok_or(Failure::Disconnected)?.map_err(connection_failure)?;
                let packet: Packet = match frame {
                    Message::Text(text) => serde_json::from_str(&text).map_err(|_| Failure::Protocol)?,
                    Message::Ping(bytes) => { socket.send(Message::Pong(bytes)).await?; continue; },
                    Message::Pong(_) => continue,
                    Message::Close(close) => bail!(close.map(|c| Failure::Closed(u16::from(c.code))).unwrap_or(Failure::Disconnected)),
                    _ => bail!(Failure::Protocol),
                };
                if packet.op == "cancel" { bail!(Failure::Canceled); }
                stage.accept(&packet.op)?;
                match packet.op.as_str() {
                    "hello" => {
                        ensure!(!hello, "Repeated hello");
                        let (interval, timeout) = negotiated_timing(packet.heartbeat_interval, packet.timeout_ms)?;
                        deadline = tokio::time::Instant::now() + timeout;
                        heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
                        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        socket.send(Message::Text(serde_json::json!({"op":"init","encoded_public_key":STANDARD.encode(der.as_bytes())}).to_string().into())).await?;
                        hello = true;
                    },
                    "nonce_proof" => {
                        ensure!(hello && !nonce_verified, "Unexpected nonce");
                        let nonce = decrypt(&key, packet.encrypted_nonce.as_deref().map(|n| n.as_str()).ok_or(Failure::Protocol)?).map_err(|_| Failure::Crypto)?;
                        let proof = Zeroizing::new(URL_SAFE_NO_PAD.encode(nonce.as_slice()));
                        socket.send(Message::Text(format!("{{\"op\":\"nonce_proof\",\"nonce\":\"{}\"}}", proof.as_str()).into())).await?;
                        nonce_verified = true;
                    },
                    "pending_remote_init" => {
                        ensure!(nonce_verified && !code_ready, "Unexpected fingerprint");
                        let fingerprint = packet.fingerprint.ok_or_else(|| anyhow::anyhow!("Missing fingerprint"))?;
                        ensure!(fingerprint_matches(der.as_bytes(), &fingerprint), Failure::Fingerprint);
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
                            .body(payload.to_vec()).send().await.map_err(|_| Failure::ExchangeNetwork)?;
                        if !response.status().is_success() { bail!(Failure::ExchangeHttp(response.status().as_u16())); }
                        #[derive(Deserialize)]
                        struct TokenResponse { encrypted_token: Zeroizing<String> }
                        let mut buffer = Zeroizing::new(Vec::new());
                        while let Some(chunk) = response.chunk().await? {
                            ensure!(buffer.len() + chunk.len() <= 16 * 1024, "Oversized response");
                            buffer.extend_from_slice(&chunk);
                        }
                        let body: TokenResponse = serde_json::from_slice(&buffer)?;
                        let bytes = decrypt(&key, &body.encrypted_token).map_err(|_| Failure::Crypto)?;
                        let token = Zeroizing::new(String::from_utf8(bytes.to_vec())?);
                        ensure!(!token.is_empty() && token.len() < 4096 && !token.contains(['\r','\n']), "Invalid credential");
                        let _ = socket.close(None).await;
                        notify(tx, ctx, Event::Token(token));
                        return Ok(());
                    },
                    "heartbeat_ack" => { ensure!(awaiting_ack, "Unexpected heartbeat ack"); awaiting_ack = false; },
                    "cancel" => bail!(Failure::Canceled),
                    _ => bail!("Unknown protocol event"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires network; reads hello only, never sends init or requests a QR/session"]
    async fn remote_gateway_hello_without_starting_login() {
        let mut socket = connect_gateway().await.unwrap_or_else(|error| {
            panic!(
                "{}",
                error
                    .downcast_ref::<Failure>()
                    .copied()
                    .unwrap_or(Failure::Protocol)
            );
        });
        let frame = tokio::time::timeout(Duration::from_secs(15), socket.next()).await;
        let packet = match frame {
            Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<Packet>(&text).ok(),
            _ => None,
        };
        let _ = socket.close(None).await;
        let packet = packet.expect("No valid hello received; payload withheld");
        assert_eq!(packet.op, "hello");
        assert!(negotiated_timing(packet.heartbeat_interval, packet.timeout_ms).is_ok());
        println!(
            "Public hello: heartbeat_interval={:?}, timeout_ms={:?}; no init, QR or account request sent",
            packet.heartbeat_interval, packet.timeout_ms
        );
    }
    #[test]
    fn longer_server_login_lifetime_is_capped_locally_instead_of_rejected() {
        let (heartbeat, timeout) = negotiated_timing(Some(41250), Some(308377)).unwrap();
        assert_eq!(heartbeat, Duration::from_millis(41250));
        assert_eq!(timeout, Duration::from_secs(180));
        assert_eq!(
            negotiated_timing(Some(41250), Some(u64::MAX)).unwrap().1,
            timeout
        );
        assert_eq!(
            negotiated_timing(Some(41250), Some(142637)).unwrap().1,
            Duration::from_millis(142637)
        );
        assert!(negotiated_timing(None, Some(308377)).is_err());
        assert!(negotiated_timing(Some(41250), Some(0)).is_err());
        assert!(negotiated_timing(Some(0), Some(308377)).is_err());
    }
    #[test]
    fn failure_messages_never_echo_server_content_or_invent_challenges() {
        use tokio_tungstenite::tungstenite::{Error, http::Response};
        let response = Response::builder()
            .status(403)
            .body(Some(b"fixture-secret-ticket-and-server-message".to_vec()))
            .unwrap();
        let message = connection_failure(Error::Http(response)).message();
        assert!(message.contains("HTTP 403"));
        assert!(!message.contains("fixture-secret"));
        assert!(!message.contains("security challenge"));
        assert!(
            Failure::ExchangeHttp(429)
                .message()
                .contains("rate-limited")
        );
        assert!(Failure::Closed(4003).message().contains("expired"));
        assert!(Failure::Fingerprint.message().contains("fingerprint"));
    }
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
