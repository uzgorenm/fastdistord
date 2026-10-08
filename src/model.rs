use crate::messaging::{ChatMessage, TextChannel};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: u64,
    pub avatar: Option<String>,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guild {
    pub id: u64,
    pub icon: Option<String>,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub id: u64,
    pub guild_id: u64,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    pub id: u64,
    pub name: String,
    pub speaking: bool,
    pub muted: bool,
    pub deafened: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceChoice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Offline,
    Connecting,
    SignalingReady,
    Joining,
    VoiceReady,
    VoiceWaiting,
    Reconnecting,
    Failed,
}
#[derive(Clone, Debug)]
pub struct UiState {
    pub account: Option<Account>,
    pub profiles: std::collections::HashMap<u64, crate::profiles::Profile>,
    pub guilds: Vec<Guild>,
    pub channels: Vec<Channel>,
    pub participants: Vec<Participant>,
    pub friends: Vec<crate::social::Friend>,
    pub direct_channels: Vec<crate::social::DirectChannel>,
    pub selected_dm: Option<u64>,
    pub selected_call_dm: Option<u64>,
    pub selected_call_guild: Option<u64>,
    pub social_busy: bool,
    pub social_status: String,
    pub phase: Phase,
    pub voice_handshake: Arc<songbird::DaveHandshake>,
    pub status: String,
    pub selected_guild: Option<u64>,
    pub selected_channel: Option<u64>,
    pub muted: bool,
    pub deafened: bool,
    pub ptt_enabled: bool,
    pub input_level: f32,
    pub output_volume: f32,
    pub revision: u64,
    pub input_devices: Vec<DeviceChoice>,
    pub output_devices: Vec<DeviceChoice>,
    pub selected_input: Option<String>,
    pub selected_output: Option<String>,
    pub audio_diagnostics: String,
    pub text_channels: Vec<TextChannel>,
    pub selected_text_channel: Option<u64>,
    pub messages: Vec<ChatMessage>,
    pub chat_busy: bool,
    pub chat_sending: bool,
    pub chat_status: String,
    pub sent_revision: u64,
}
impl Default for UiState {
    fn default() -> Self {
        Self {
            account: None,
            profiles: Default::default(),
            guilds: vec![],
            channels: vec![],
            participants: vec![],
            friends: vec![],
            direct_channels: vec![],
            selected_dm: None,
            selected_call_dm: None,
            selected_call_guild: None,
            social_busy: false,
            social_status: String::new(),
            phase: Phase::Offline,
            voice_handshake: Arc::new(songbird::DaveHandshake::default()),
            status: String::new(),
            selected_guild: None,
            selected_channel: None,
            muted: true,
            deafened: false,
            ptt_enabled: false,
            input_level: 0.0,
            output_volume: 1.0,
            revision: 0,
            input_devices: vec![],
            output_devices: vec![],
            selected_input: None,
            selected_output: None,
            audio_diagnostics: "No active audio devices".into(),
            text_channels: vec![],
            selected_text_channel: None,
            messages: vec![],
            chat_busy: false,
            chat_sending: false,
            chat_status: "Choose a text channel to read its latest 50 messages.".into(),
            sent_revision: 0,
        }
    }
}
// Deliberately no Debug: Connect contains an ephemeral secret.
pub enum Command {
    Connect {
        token: String,
        risk_accepted: bool,
        remember: bool,
    },
    #[cfg(target_os = "macos")]
    ConnectSaved {
        risk_accepted: bool,
    },
    Logout,
    Reconnect,
    SelectHome,
    OpenDm(u64),
    SelectDm(u64),
    CallDm(u64),
    SelectGuild(u64),
    SelectTextChannel(u64),
    RefreshMessages,
    SendMessage {
        channel_id: u64,
        content: String,
    },
    Join {
        guild_id: u64,
        channel_id: u64,
    },
    Leave,
    SetMuted(bool),
    SetDeafened(bool),
    SetPtt(bool),
    SetOutputVolume(f32),
    RefreshDevices,
    SetDevices {
        input: Option<String>,
        output: Option<String>,
    },
    SetUiRepaint(Arc<dyn Fn() + Send + Sync>),
    SetUiVisible(bool),
    Quit,
}
