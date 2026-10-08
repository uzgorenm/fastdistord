//! Isolated, explicitly opted-in unofficial personal-account adapter.
//! This is not Discord's bot API, an approved OAuth integration, or a promise of account safety.
use crate::messaging::{self, ChatMessage, TextChannel};
use crate::model::{Account, Channel, Guild};
use crate::social::{self, DirectChannel, Friend};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use zeroize::Zeroizing;

pub struct PersonalAccount {
    token: Zeroizing<String>,
    http: reqwest::Client,
}
#[derive(Debug)]
pub struct AuthenticationRejected;
impl std::fmt::Display for AuthenticationRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Discord rejected this login. Sign in again.")
    }
}
impl std::error::Error for AuthenticationRejected {}

impl PersonalAccount {
    pub async fn connect(
        token: String,
        risk_accepted: bool,
    ) -> Result<(Self, Account, Vec<Guild>)> {
        let token = Zeroizing::new(token);
        if !risk_accepted {
            bail!("Unofficial personal-account access requires explicit risk acceptance.");
        }
        if token.trim().is_empty() || token.starts_with("Bot ") || token.contains(['\r', '\n']) {
            bail!("Enter a personal account credential locally; bot credentials are unsupported.");
        }
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .user_agent(format!("fastdistord/{}", fastdistord::RELEASE_VERSION))
            .build()?;
        let client = Self { token, http };
        let user = client.get("/users/@me").await?;
        let account = Account {
            avatar: crate::profiles::hash(&user["avatar"]),
            id: snowflake(&user["id"]).context("Account response had no valid user ID")?,
            name: display_name(&user),
        };
        let values = client.get("/users/@me/guilds").await?;
        let guilds = values
            .as_array()
            .context("Invalid guild response")?
            .iter()
            .take(250)
            .filter_map(|v| {
                Some(Guild {
                    icon: crate::profiles::hash(&v["icon"]),
                    id: snowflake(&v["id"])?,
                    name: v["name"].as_str()?.to_owned(),
                })
            })
            .collect();
        Ok((client, account, guilds))
    }
    async fn get(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::GET, path, None).await
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self
            .http
            .request(method, format!("https://discord.com/api/v10{path}"))
            .header(reqwest::header::AUTHORIZATION, self.token.as_str());
        if let Some(body) = body {
            request = request.json(&body);
        }
        // Never propagate HTTP error sources: they may include server-controlled
        // content or URLs. Never retry a POST after an uncertain outcome.
        let mut response = request.send().await.map_err(|_| {
            anyhow::anyhow!("Discord request failed; its outcome may be uncertain.")
        })?;
        match response.status().as_u16() {
            401 => return Err(AuthenticationRejected.into()),
            403 => {
                bail!("Discord denied account or channel access. No retry or bypass was attempted.")
            }
            429 => bail!("Discord rate limited the request. Wait before trying again."),
            200 | 201 => {}
            204 => return Ok(Value::Null),
            _ => bail!(
                "Discord request failed with HTTP {}",
                response.status().as_u16()
            ),
        }
        const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
        if response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            bail!("Discord response exceeded the safety limit");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            anyhow::anyhow!("Discord response could not be read; its outcome may be uncertain.")
        })? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                bail!("Discord response exceeded the safety limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Discord returned an invalid response."))
    }
    /// One bounded snapshot; never sends friend requests or changes relationships.
    pub async fn friends(&self) -> Result<Vec<Friend>> {
        social::parse_friends(&self.get("/users/@me/relationships").await?)
    }
    pub async fn direct_channels(&self) -> Result<social::DirectChannelsSnapshot> {
        social::parse_direct_channels(&self.get("/users/@me/channels").await?)
    }
    /// Only from selecting a friend. Reuses an existing DM when present.
    pub async fn open_dm(&self, user_id: u64) -> Result<DirectChannel> {
        messaging::validate_id(user_id)?;
        let value = self
            .request(
                reqwest::Method::POST,
                "/users/@me/channels",
                Some(json!({"recipient_id": user_id.to_string()})),
            )
            .await?;
        let channel = social::parse_direct_channel(&value)?;
        if value["type"].as_u64() != Some(1)
            || channel.recipients.len() != 1
            || channel.recipients[0].id != user_id
        {
            bail!("Discord returned a different conversation than requested.");
        }
        Ok(channel)
    }
    /// An explicit Call action may ring these recipients only after a call exists.
    /// No automatic ring, retry or arbitrary recipient notifications.
    pub async fn ring(&self, channel_id: u64, recipient_ids: &[u64]) -> Result<()> {
        messaging::validate_id(channel_id)?;
        let recipients = ring_payload(recipient_ids)?;
        self.request(
            reqwest::Method::POST,
            &format!("/channels/{channel_id}/call/ring"),
            Some(recipients),
        )
        .await?;
        Ok(())
    }
    /// One best-effort cancellation of the recipients of our explicit call.
    pub async fn stop_ringing(&self, channel_id: u64, recipient_ids: &[u64]) -> Result<()> {
        messaging::validate_id(channel_id)?;
        let recipients = ring_payload(recipient_ids)?;
        self.request(
            reqwest::Method::POST,
            &format!("/channels/{channel_id}/call/stop-ringing"),
            Some(recipients),
        )
        .await?;
        Ok(())
    }
    pub async fn text_channels(&self, guild_id: u64) -> Result<Vec<TextChannel>> {
        messaging::validate_id(guild_id)?;
        let value = self.get(&format!("/guilds/{guild_id}/channels")).await?;
        messaging::parse_channels(&value, guild_id)
    }
    /// A bounded snapshot, oldest first. No background polling or pagination.
    pub async fn messages(&self, channel_id: u64) -> Result<Vec<ChatMessage>> {
        messaging::validate_id(channel_id)?;
        let value = self
            .get(&format!("/channels/{channel_id}/messages?limit=50"))
            .await?;
        messaging::parse_messages(&value, channel_id)
    }
    /// Only invoke from an explicit send action. No retries, attachments or mentions.
    pub async fn send_message(&self, channel_id: u64, content: &str) -> Result<ChatMessage> {
        messaging::validate_id(channel_id)?;
        let body = messaging::send_payload(content)?;
        let value = self
            .request(
                reqwest::Method::POST,
                &format!("/channels/{channel_id}/messages"),
                Some(body),
            )
            .await?;
        messaging::parse_message(&value, channel_id).map_err(|_| {
            anyhow::anyhow!("Discord returned an invalid send response. The message may have been sent; check history before retrying.")
        })
    }
    pub async fn channels(&self, guild_id: u64) -> Result<Vec<Channel>> {
        let v = self.get(&format!("/guilds/{guild_id}/channels")).await?;
        Ok(v.as_array()
            .context("Invalid channels response")?
            .iter()
            .take(1000)
            .filter(|v| v["type"].as_u64() == Some(2))
            .filter_map(|v| {
                Some(Channel {
                    id: snowflake(&v["id"])?,
                    guild_id,
                    name: v["name"].as_str()?.into(),
                })
            })
            .collect())
    }
    pub fn gateway(&self) -> Gateway {
        Gateway::spawn(Zeroizing::new(self.token.to_string()), None)
    }
    pub fn resume_gateway(&self, resume: GatewayResume) -> Gateway {
        Gateway::spawn(Zeroizing::new(self.token.to_string()), Some(resume))
    }
}
pub fn snowflake(value: &Value) -> Option<u64> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse().ok())
        .or_else(|| value.as_u64())
        .filter(|id| *id != 0)
}
pub fn display_name(user: &Value) -> String {
    user["global_name"]
        .as_str()
        .or(user["username"].as_str())
        .unwrap_or("Participant")
        .chars()
        .take(100)
        .collect()
}
pub enum GatewayCommand {
    Voice {
        guild_id: u64,
        channel_id: Option<u64>,
        muted: bool,
        deafened: bool,
    },
    PrivateVoice {
        channel_id: Option<u64>,
        muted: bool,
        deafened: bool,
    },
    RequestCall {
        channel_id: u64,
    },
    Close,
}
#[derive(Clone)]
pub struct GatewayResume {
    session_id: Zeroizing<String>,
    sequence: u64,
    url: String,
}
#[derive(Debug)]
struct TerminalGateway;
impl std::fmt::Display for TerminalGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gateway requires explicit account reconnection")
    }
}
impl std::error::Error for TerminalGateway {}
pub enum GatewayEvent {
    Ready {
        resumed: bool,
    },
    SelfPresence(crate::profiles::Presence),
    RosterBegin(u64),
    RosterComplete(u64, bool),
    Dispatch {
        kind: String,
        data: Value,
    },
    Closed {
        message: String,
        retryable: bool,
        auth_rejected: bool,
        resume: Option<GatewayResume>,
    },
}
pub struct Gateway {
    pub commands: mpsc::Sender<GatewayCommand>,
    pub events: mpsc::Receiver<GatewayEvent>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Gateway {
    fn spawn(token: Zeroizing<String>, resume: Option<GatewayResume>) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let (event_tx, event_rx) = mpsc::channel(128);
        let task = tokio::spawn(async move {
            let mut session = resume;
            if let Err(error) = gateway_loop(token, rx, event_tx.clone(), &mut session).await {
                let retryable = error.downcast_ref::<TerminalGateway>().is_none()
                    && error.downcast_ref::<AuthenticationRejected>().is_none()
                    && session.is_some();
                let _ = event_tx
                    .send(GatewayEvent::Closed {
                        message: if retryable {
                            "Discord signaling interrupted; attempting a bounded session resume."
                        } else {
                            "Discord signaling stopped; explicit account reconnection is required."
                        }
                        .into(),
                        retryable,
                        auth_rejected: error.downcast_ref::<AuthenticationRejected>().is_some(),
                        resume: if retryable { session } else { None },
                    })
                    .await;
            }
        });
        Self {
            commands: tx,
            events: event_rx,
            task,
        }
    }
}
async fn gateway_loop(
    token: Zeroizing<String>,
    mut commands: mpsc::Receiver<GatewayCommand>,
    events: mpsc::Sender<GatewayEvent>,
    session: &mut Option<GatewayResume>,
) -> Result<()> {
    let (socket, _) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        connect_async_with_config(
            session
                .as_ref()
                .map(|r| r.url.as_str())
                .unwrap_or("wss://gateway.discord.gg/?v=10&encoding=json"),
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(8 * 1024 * 1024))
                    .max_frame_size(Some(8 * 1024 * 1024)),
            ),
            true,
        ),
    )
    .await??;
    let (mut write, mut read) = socket.split();
    let first = tokio::time::timeout(std::time::Duration::from_secs(15), read.next())
        .await?
        .context("Gateway closed")??;
    let hello: Value = serde_json::from_str(first.to_text().map_err(|_| TerminalGateway)?)
        .map_err(|_| TerminalGateway)?;
    if hello["op"] != 10 {
        return Err(TerminalGateway.into());
    }
    let ms = hello["d"]["heartbeat_interval"]
        .as_u64()
        .filter(|n| (1000..=120000).contains(n))
        .ok_or(TerminalGateway)?;
    // User-account wire format is undocumented and may change. No bot intents or spoofed official build.
    let identify = match session.as_ref() {
        Some(resume) => {
            json!({"op":6,"d":{"token":token.as_str(),"session_id":resume.session_id.as_str(),"seq":resume.sequence}})
        }
        None => {
            json!({"op":2,"d":{"token":token.as_str(),"properties":{"os":std::env::consts::OS,"browser":"fastdistord","device":"fastdistord"},"compress":false,"large_threshold":50}})
        }
    };
    write
        .send(Message::Text(identify.to_string().into()))
        .await?;
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_millis(ms));
    let mut seq: Option<u64> = session.as_ref().map(|r| r.sequence);
    let mut ack = true;
    enum Incoming {
        Heartbeat,
        Command(Option<GatewayCommand>),
        Message(Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>),
    }
    loop {
        let event = tokio::select! {
            _=heartbeat.tick()=>Incoming::Heartbeat,
            command=commands.recv()=>Incoming::Command(command),
            incoming=read.next()=>Incoming::Message(incoming),
        };
        match event {
            Incoming::Heartbeat => {
                if !ack {
                    bail!("Heartbeat acknowledgement timed out");
                }
                ack = false;
                write
                    .send(Message::Text(json!({"op":1,"d":seq}).to_string().into()))
                    .await?;
            }
            Incoming::Command(command) => match command {
                Some(GatewayCommand::Voice {
                    guild_id,
                    channel_id,
                    muted,
                    deafened,
                }) => {
                    let update = json!({"op":4,"d":{"guild_id":guild_id.to_string(),"channel_id":channel_id.map(|id|id.to_string()),"self_mute":muted,"self_deaf":deafened}});
                    write.send(Message::Text(update.to_string().into())).await?;
                }
                Some(GatewayCommand::PrivateVoice {
                    channel_id,
                    muted,
                    deafened,
                }) => {
                    if channel_id == Some(0) {
                        bail!("Choose a valid conversation.");
                    }
                    write
                        .send(Message::Text(
                            private_voice_payload(channel_id, muted, deafened)
                                .to_string()
                                .into(),
                        ))
                        .await?;
                }
                Some(GatewayCommand::RequestCall { channel_id }) => {
                    messaging::validate_id(channel_id)?;
                    write
                        .send(Message::Text(
                            json!({"op":13,"d":{"channel_id":channel_id.to_string()}})
                                .to_string()
                                .into(),
                        ))
                        .await?;
                }
                Some(GatewayCommand::Close) | None => {
                    let _ = write.close().await;
                    return Ok(());
                }
            },
            Incoming::Message(incoming) => {
                let message = incoming.context("Gateway closed")??;
                match message {
                    Message::Text(text) => {
                        if text.len() > 8 * 1024 * 1024 {
                            bail!("Gateway frame too large");
                        }
                        let v: Value = serde_json::from_str(&text).map_err(|_| TerminalGateway)?;
                        if let Some(s) = v["s"].as_u64() {
                            seq = Some(s);
                            if let Some(resume) = session.as_mut() {
                                resume.sequence = s;
                            }
                        }
                        match v["op"].as_u64() {
                            Some(11) => ack = true,
                            Some(1) => {
                                write
                                    .send(Message::Text(json!({"op":1,"d":seq}).to_string().into()))
                                    .await?
                            }
                            Some(7) => bail!("Gateway requested resume"),
                            Some(9) => return Err(TerminalGateway.into()),
                            Some(0) => {
                                let kind = v["t"].as_str().unwrap_or_default();
                                if kind == "READY" {
                                    if let (Some(id), Some(sequence)) =
                                        (v["d"]["session_id"].as_str(), seq)
                                    {
                                        *session = Some(GatewayResume {
                                            session_id: Zeroizing::new(id.to_owned()),
                                            sequence,
                                            url: validated_resume_url(
                                                v["d"]["resume_gateway_url"].as_str(),
                                            )
                                            .ok_or(TerminalGateway)?,
                                        });
                                    } else {
                                        return Err(TerminalGateway.into());
                                    }
                                    events.send(GatewayEvent::Ready { resumed: false }).await?;
                                    events
                                        .send(GatewayEvent::SelfPresence(
                                            crate::profiles::self_presence(&v["d"]["sessions"]),
                                        ))
                                        .await?;
                                    emit_presences(&v["d"], &events).await?;
                                    if let Some(guilds) = v["d"]["guilds"].as_array() {
                                        for guild in guilds.iter().take(250) {
                                            emit_initial_voice_states(guild, &events).await?;
                                            emit_presences(guild, &events).await?;
                                        }
                                    }
                                }
                                if kind == "RESUMED" {
                                    events.send(GatewayEvent::Ready { resumed: true }).await?;
                                }
                                if kind == "GUILD_CREATE" {
                                    emit_initial_voice_states(&v["d"], &events).await?;
                                    emit_presences(&v["d"], &events).await?;
                                }
                                if kind == "SESSIONS_REPLACE" {
                                    events
                                        .send(GatewayEvent::SelfPresence(
                                            crate::profiles::self_presence(&v["d"]),
                                        ))
                                        .await?;
                                }
                                if matches!(
                                    kind,
                                    "PRESENCE_UPDATE"
                                        | "USER_UPDATE"
                                        | "VOICE_STATE_UPDATE"
                                        | "VOICE_SERVER_UPDATE"
                                        | "GUILD_DELETE"
                                        | "CHANNEL_DELETE"
                                        | "CHANNEL_UPDATE"
                                        | "GUILD_MEMBER_UPDATE"
                                        | "GUILD_ROLE_UPDATE"
                                        | "GUILD_ROLE_DELETE"
                                        | "GUILD_UPDATE"
                                        | "CALL_CREATE"
                                        | "CALL_UPDATE"
                                        | "CALL_DELETE"
                                        | "RELATIONSHIP_ADD"
                                        | "RELATIONSHIP_UPDATE"
                                        | "RELATIONSHIP_REMOVE"
                                        | "CHANNEL_CREATE"
                                        | "MESSAGE_CREATE"
                                        | "MESSAGE_UPDATE"
                                        | "MESSAGE_DELETE"
                                ) {
                                    events
                                        .send(GatewayEvent::Dispatch {
                                            kind: kind.into(),
                                            data: v["d"].clone(),
                                        })
                                        .await?;
                                }
                            }
                            _ => {}
                        }
                    }
                    Message::Ping(v) => write.send(Message::Pong(v)).await?,
                    Message::Close(frame) => {
                        if frame.as_ref().is_some_and(|f| u16::from(f.code) == 4004) {
                            return Err(AuthenticationRejected.into());
                        }
                        if !gateway_close_is_retryable(frame.as_ref().map(|f| u16::from(f.code))) {
                            return Err(TerminalGateway.into());
                        }
                        bail!("Gateway transport closed");
                    }
                    _ => {}
                }
            }
        }
    }
}
// Reverse-engineered private call protocol: guild_id must remain JSON null.
fn private_voice_payload(channel_id: Option<u64>, muted: bool, deafened: bool) -> Value {
    json!({"op":4,"d":{"guild_id":null,"channel_id":channel_id.map(|id|id.to_string()),
        "self_mute":muted,"self_deaf":deafened}})
}
fn ring_payload(recipient_ids: &[u64]) -> Result<Value> {
    if recipient_ids.is_empty() || recipient_ids.len() > 25 {
        bail!("Choose the call recipients before ringing.");
    }
    let mut seen = std::collections::HashSet::new();
    for id in recipient_ids {
        messaging::validate_id(*id)?;
        if !seen.insert(*id) {
            bail!("Call recipients must be unique.");
        }
    }
    Ok(json!({"recipients":recipient_ids.iter().map(u64::to_string).collect::<Vec<_>>()}))
}
fn validated_resume_url(value: Option<&str>) -> Option<String> {
    let url = reqwest::Url::parse(value?).ok()?;
    let host = url.host_str()?;
    if url.scheme() != "wss"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|p| p != 443)
        || !(host == "gateway.discord.gg"
            || (host.starts_with("gateway-") && host.ends_with(".discord.gg")))
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(format!("wss://{host}/?v=10&encoding=json"))
}
fn gateway_close_is_retryable(code: Option<u16>) -> bool {
    // Only transport-level closures. Unknown application codes, invalid sessions,
    // auth failures and rate limits require explicit action, never fresh identify.
    matches!(code, None | Some(1001 | 1006 | 1011 | 1012 | 1013 | 4000))
}
/// Forward only voice/member-name data, not the full READY guild payload.
async fn emit_initial_voice_states(
    guild: &Value,
    events: &mpsc::Sender<GatewayEvent>,
) -> Result<()> {
    let Some(guild_id) = snowflake(&guild["id"]) else {
        return Ok(());
    };
    let Some(states) = guild["voice_states"].as_array() else {
        return Ok(());
    };
    events.send(GatewayEvent::RosterBegin(guild_id)).await?;
    let complete = states.len() <= 250
        && states
            .iter()
            .all(|v| snowflake(&v["user_id"]).is_some() && snowflake(&v["channel_id"]).is_some());
    for state in states.iter().take(250) {
        let mut state = state.clone();
        state["guild_id"] = Value::String(guild_id.to_string());
        if state["member"].is_null()
            && let Some(user_id) = snowflake(&state["user_id"])
            && let Some(members) = guild["members"].as_array()
            && let Some(member) = members
                .iter()
                .take(5000)
                .find(|m| snowflake(&m["user"]["id"]) == Some(user_id))
        {
            state["member"] = member.clone();
        }
        events
            .send(GatewayEvent::Dispatch {
                kind: "VOICE_STATE_UPDATE".into(),
                data: state,
            })
            .await?;
    }
    events
        .send(GatewayEvent::RosterComplete(guild_id, complete))
        .await?;
    Ok(())
}
async fn emit_presences(data: &Value, events: &mpsc::Sender<GatewayEvent>) -> Result<()> {
    let lists = [
        data.get("presences"),
        data.get("merged_presences").and_then(|m| m.get("friends")),
    ];
    for list in lists.into_iter().flatten().filter_map(Value::as_array) {
        for p in list.iter().take(1000) {
            if let Some(id) = snowflake(&p["user"]["id"]) {
                // Retain neither activities nor session identifiers. Partial
                // presence users must not erase an existing avatar.
                let mut user = json!({"id":id.to_string()});
                if let Some(avatar) = p["user"].get("avatar") {
                    user["avatar"] = avatar.clone();
                }
                events
                    .send(GatewayEvent::Dispatch {
                        kind: "PRESENCE_UPDATE".into(),
                        data: json!({"user":user,"status":p["status"]}),
                    })
                    .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_calls_do_not_impersonate_guild_voice_or_ring_everyone() {
        let payload = private_voice_payload(Some(42), true, false);
        assert!(payload["d"]["guild_id"].is_null());
        assert_eq!(payload["d"]["channel_id"], "42");
        assert!(private_voice_payload(None, true, true)["d"]["channel_id"].is_null());
        assert_eq!(ring_payload(&[42]).unwrap(), json!({"recipients":["42"]}));
        for recipients in [vec![], vec![0], vec![42, 42], vec![1; 26]] {
            assert!(ring_payload(&recipients).is_err());
        }
    }
    #[test]
    fn snowflakes_reject_zero_and_garbage() {
        assert_eq!(snowflake(&json!("123")), Some(123));
        assert_eq!(snowflake(&json!("0")), None);
        assert_eq!(snowflake(&json!("secret")), None);
        assert_eq!(snowflake(&json!("+123")), None);
        assert_eq!(snowflake(&json!(" 123")), None);
        assert_eq!(snowflake(&json!("18446744073709551616")), None);
    }
    #[tokio::test]
    async fn opt_in_is_required_before_network() {
        let result = PersonalAccount::connect("never-transmitted".into(), false).await;
        assert!(result.is_err());
    }
    #[test]
    fn gateway_retry_classification_never_reauthenticates_or_bypasses_limits() {
        for code in [
            4003, 4004, 4007, 4008, 4009, 4010, 4011, 4012, 4013, 4014, 1000,
        ] {
            assert!(!gateway_close_is_retryable(Some(code)));
        }
        for code in [1001, 1006, 1011, 1012, 1013, 4000] {
            assert!(gateway_close_is_retryable(Some(code)));
        }
        assert!(gateway_close_is_retryable(None));
    }
    #[test]
    fn resume_credentials_are_restricted_to_discord_gateway() {
        assert!(validated_resume_url(Some("wss://gateway-us-east1-b.discord.gg")).is_some());
        for url in [
            "wss://evil.example",
            "wss://gateway.discord.gg.evil.example",
            "wss://user:secret@gateway.discord.gg",
            "ws://gateway.discord.gg",
            "wss://gateway.discord.gg/?token=secret",
            "wss://gateway.discord.gg:444",
        ] {
            assert!(validated_resume_url(Some(url)).is_none(), "{url}");
        }
    }
}
