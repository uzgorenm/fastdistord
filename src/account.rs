//! Isolated, explicitly opted-in unofficial personal-account adapter.
//! This is not Discord's bot API, an approved OAuth integration, or a promise of account safety.
use crate::model::{Account, Channel, Guild};
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
            .user_agent("fastdistord/0.1 experimental personal voice client")
            .build()?;
        let client = Self { token, http };
        let user = client.get("/users/@me").await?;
        let account = Account {
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
                    id: snowflake(&v["id"])?,
                    name: v["name"].as_str()?.to_owned(),
                })
            })
            .collect();
        Ok((client, account, guilds))
    }
    async fn get(&self, path: &str) -> Result<Value> {
        let mut response = self
            .http
            .get(format!("https://discord.com/api/v10{path}"))
            .header(reqwest::header::AUTHORIZATION, self.token.as_str())
            .send()
            .await
            .context("Discord request failed")?;
        match response.status().as_u16() {
            401 | 403 => {
                bail!("Discord denied account or channel access. No retry or bypass was attempted.")
            }
            429 => bail!("Discord rate limited the request. Wait before trying again."),
            200 => {}
            _ => bail!(
                "Discord request failed with HTTP {}",
                response.status().as_u16()
            ),
        }
        if response
            .content_length()
            .is_some_and(|n| n > 4 * 1024 * 1024)
        {
            bail!("Discord response exceeded the safety limit");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
                bail!("Discord response exceeded the safety limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&bytes)?)
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
        Gateway::spawn(Zeroizing::new(self.token.to_string()))
    }
}
pub fn snowflake(value: &Value) -> Option<u64> {
    value
        .as_str()
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
    Close,
}
pub enum GatewayEvent {
    Ready,
    Dispatch { kind: String, data: Value },
    Closed(String),
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
    fn spawn(token: Zeroizing<String>) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let (event_tx, event_rx) = mpsc::channel(128);
        let task = tokio::spawn(async move {
            if gateway_loop(token, rx, event_tx.clone()).await.is_err() {
                let _=event_tx.send(GatewayEvent::Closed("Discord signaling disconnected. Reconnect your account; microphone has been stopped.".into())).await;
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
) -> Result<()> {
    let (socket, _) = connect_async_with_config(
        "wss://gateway.discord.gg/?v=10&encoding=json",
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(8 * 1024 * 1024))
                .max_frame_size(Some(8 * 1024 * 1024)),
        ),
        true,
    )
    .await?;
    let (mut write, mut read) = socket.split();
    let first = tokio::time::timeout(std::time::Duration::from_secs(15), read.next())
        .await?
        .context("Gateway closed")??;
    let hello: Value = serde_json::from_str(first.to_text()?)?;
    if hello["op"] != 10 {
        bail!("Expected Gateway Hello");
    }
    let ms = hello["d"]["heartbeat_interval"]
        .as_u64()
        .filter(|n| (1000..=120000).contains(n))
        .context("Invalid heartbeat interval")?;
    // User-account wire format is undocumented and may change. No bot intents or spoofed official build.
    let identify = json!({"op":2,"d":{"token":token.as_str(),"properties":{"os":std::env::consts::OS,"browser":"fastdistord","device":"fastdistord"},"compress":false,"large_threshold":50}});
    write
        .send(Message::Text(identify.to_string().into()))
        .await?;
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_millis(ms));
    let mut seq: Option<u64> = None;
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
                        let v: Value = serde_json::from_str(&text)?;
                        if let Some(s) = v["s"].as_u64() {
                            seq = Some(s);
                        }
                        match v["op"].as_u64() {
                            Some(11) => ack = true,
                            Some(1) => {
                                write
                                    .send(Message::Text(json!({"op":1,"d":seq}).to_string().into()))
                                    .await?
                            }
                            Some(7) | Some(9) => {
                                bail!("Gateway requested reconnect or rejected session")
                            }
                            Some(0) => {
                                let kind = v["t"].as_str().unwrap_or_default();
                                if kind == "READY" {
                                    events.send(GatewayEvent::Ready).await?;
                                    if let Some(guilds) = v["d"]["guilds"].as_array() {
                                        for guild in guilds.iter().take(250) {
                                            emit_initial_voice_states(guild, &events).await?;
                                        }
                                    }
                                }
                                if kind == "GUILD_CREATE" {
                                    emit_initial_voice_states(&v["d"], &events).await?;
                                }
                                if matches!(
                                    kind,
                                    "VOICE_STATE_UPDATE"
                                        | "VOICE_SERVER_UPDATE"
                                        | "GUILD_DELETE"
                                        | "CHANNEL_DELETE"
                                        | "CHANNEL_UPDATE"
                                        | "GUILD_MEMBER_UPDATE"
                                        | "GUILD_ROLE_UPDATE"
                                        | "GUILD_ROLE_DELETE"
                                        | "GUILD_UPDATE"
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
                    Message::Close(_) => bail!("Gateway closed"),
                    _ => {}
                }
            }
        }
    }
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
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snowflakes_reject_zero_and_garbage() {
        assert_eq!(snowflake(&json!("123")), Some(123));
        assert_eq!(snowflake(&json!("0")), None);
        assert_eq!(snowflake(&json!("secret")), None);
    }
    #[tokio::test]
    async fn opt_in_is_required_before_network() {
        let result = PersonalAccount::connect("never-transmitted".into(), false).await;
        assert!(result.is_err());
    }
}
