use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: u64,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guild {
    pub id: u64,
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
    Reconnecting,
    Failed,
}
#[derive(Clone, Debug)]
pub struct UiState {
    pub account: Option<Account>,
    pub guilds: Vec<Guild>,
    pub channels: Vec<Channel>,
    pub participants: Vec<Participant>,
    pub phase: Phase,
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
}
impl Default for UiState {
    fn default() -> Self {
        Self {
            account: None,
            guilds: vec![],
            channels: vec![],
            participants: vec![],
            phase: Phase::Offline,
            status: "Connect an account to begin. Unofficial access is disabled until you opt in."
                .into(),
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
    SelectGuild(u64),
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
