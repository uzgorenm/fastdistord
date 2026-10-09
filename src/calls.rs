//! Local call presentation follows confirmed transport/Gateway state; never sends chat.
use crate::model::{Phase, UiState};
use std::time::{Duration, Instant};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Connecting,
    RingRequested,
    Ringing,
    JoinedPending,
    AnsweredPending,
    Active,
    Reconnecting,
}
impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::RingRequested => "Ring requested · waiting for answer",
            Self::Ringing => "Ringing",
            Self::JoinedPending => "Joined · encryption pending",
            Self::AnsweredPending => "Answered · encryption pending",
            Self::Active => "Voice connected",
            Self::Reconnecting => "Reconnecting",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cue {
    #[default]
    Stop,
    Connecting,
    Ring,
    Joined,
    Left,
}
/// One actual Gateway ring, scoped to the authenticated account and known DM.
#[derive(Clone, Debug)]
pub struct IncomingCall {
    pub channel: u64,
    pub caller_name: String,
    pub received_at: Instant,
    pub generation: u64,
    pub message_id: Option<u64>,
}
const INCOMING_TIMEOUT: Duration = Duration::from_secs(45);
pub fn incoming_matches(state: &UiState, channel: u64, generation: u64) -> bool {
    state.account.is_some()
        && state.incoming_call.as_ref().is_some_and(|call| {
            call.channel == channel
                && call.generation == generation
                && call.received_at.elapsed() < INCOMING_TIMEOUT
        })
}
pub fn clear_incoming(state: &mut UiState) -> Option<u64> {
    let call = state.incoming_call.take()?;
    state.dismissed_incoming = Some((call.channel, call.message_id));
    cue(state, Cue::Stop);
    Some(call.generation)
}
pub fn observe_incoming(state: &mut UiState, kind: &str, data: &serde_json::Value) {
    let Some(channel) = data.get("channel_id").and_then(crate::account::snowflake) else {
        return;
    };
    if kind == "CALL_DELETE" {
        if state
            .incoming_call
            .as_ref()
            .is_some_and(|call| call.channel == channel)
        {
            clear_incoming(state);
        }
        if state
            .dismissed_incoming
            .is_some_and(|(id, _)| id == channel)
        {
            state.dismissed_incoming = None;
        }
        return;
    }
    if !matches!(kind, "CALL_CREATE" | "CALL_UPDATE") {
        return;
    }
    let Some(own) = state.account.as_ref().map(|account| account.id) else {
        return;
    };
    let Some(ringing) = data
        .get("ringing")
        .and_then(serde_json::Value::as_array)
        .filter(|v| v.len() <= 25)
    else {
        return;
    };
    if !ringing
        .iter()
        .any(|value| crate::account::snowflake(value) == Some(own))
    {
        if state
            .incoming_call
            .as_ref()
            .is_some_and(|call| call.channel == channel)
        {
            clear_incoming(state);
        }
        if state
            .dismissed_incoming
            .is_some_and(|(id, _)| id == channel)
        {
            state.dismissed_incoming = None;
        }
        return;
    }
    // Partial updates for another ring cannot churn the currently shown prompt.
    if kind == "CALL_UPDATE"
        && state
            .incoming_call
            .as_ref()
            .is_some_and(|call| call.channel != channel)
    {
        return;
    }
    let message_id = data.get("message_id").and_then(crate::account::snowflake);
    if let Some((dismissed_channel, dismissed_message)) = state.dismissed_incoming
        && dismissed_channel == channel
    {
        if kind == "CALL_CREATE" && message_id.is_some() && message_id != dismissed_message {
            state.dismissed_incoming = None;
        } else {
            return;
        }
    }
    if state
        .current_call
        .as_ref()
        .is_some_and(|call| call.channel == channel)
        || state.incoming_call.as_ref().is_some_and(|call| {
            call.channel == channel
                && (kind != "CALL_CREATE" || message_id.is_none() || message_id == call.message_id)
        })
    {
        return;
    }
    let Some(dm) = state
        .direct_channels
        .iter()
        .find(|dm| dm.id == channel && dm.recipients.len() == 1)
    else {
        return;
    };
    let name = dm.name.clone();
    state.incoming_sequence = state.incoming_sequence.wrapping_add(1);
    state.incoming_call = Some(IncomingCall {
        channel,
        caller_name: name,
        received_at: Instant::now(),
        generation: state.incoming_sequence,
        message_id,
    });
    cue(state, Cue::Ring);
}
pub fn expire_incoming(state: &mut UiState, now: Instant) {
    if state
        .incoming_call
        .as_ref()
        .is_some_and(|call| now.saturating_duration_since(call.received_at) >= INCOMING_TIMEOUT)
    {
        clear_incoming(state);
    }
}
pub fn sync_sound_authority(state: &UiState) {
    let enabled = state.call_sounds
        && !state.deafened
        && !state.server_deafened
        && state.confirmed_deafened != Some(true)
        && state.account.is_some()
        && state.sound_volume.is_finite()
        && state.sound_volume > 0.0;
    state.sound_authority.store(
        if enabled {
            state.sound_sequence
        } else {
            u64::MAX
        },
        std::sync::atomic::Ordering::Release,
    );
}
#[derive(Clone, Debug)]
pub struct Call {
    pub channel: u64,
    pub private: bool,
    pub target: String,
    pub stage: Stage,
    pub confirmed: bool,
    pub ring_requested: bool,
    pub ringing: bool,
    pub peer_joined: bool,
    pub active_since: Option<Instant>,
    pub elapsed: Duration,
}
pub fn start(state: &mut UiState, channel: u64, private: bool, target: String) {
    clear_incoming(state);
    end(state, "Call canceled");
    state.current_call = Some(Call {
        channel,
        private,
        target,
        stage: Stage::Connecting,
        confirmed: false,
        ring_requested: false,
        ringing: false,
        peer_joined: false,
        active_since: None,
        elapsed: Duration::ZERO,
    });
    cue(state, Cue::Connecting);
}
pub fn server_call(state: &mut UiState, channel: u64, ringing: Option<bool>) {
    let own = state.account.as_ref().map(|a| a.id);
    let peer = state.participants.iter().any(|p| Some(p.id) != own);
    let Some(call) = &mut state.current_call else {
        return;
    };
    if !call.private || call.channel != channel {
        return;
    }
    call.confirmed = true;
    call.peer_joined = peer;
    if let Some(ringing) = ringing {
        call.ringing = ringing;
    }
    if peer {
        call.ringing = false;
    }
}
pub fn end(state: &mut UiState, _reason: &'static str) {
    if let Some(call) = state.current_call.take() {
        cue(
            state,
            if call.active_since.is_some() {
                Cue::Left
            } else {
                Cue::Stop
            },
        );
    }
}
pub fn cue(state: &mut UiState, cue: Cue) {
    state.sound_sequence = state.sound_sequence.wrapping_add(1);
    state.sound_cue = cue;
    sync_sound_authority(state);
}
pub fn refresh(state: &mut UiState) {
    let Some(call) = &mut state.current_call else {
        return;
    };
    if !matches!(
        state.phase,
        Phase::Joining | Phase::VoiceWaiting | Phase::VoiceReady | Phase::Reconnecting
    ) || state.selected_channel != Some(call.channel)
    {
        end(
            state,
            if state.phase == Phase::Failed {
                "Call failed"
            } else {
                "Call ended"
            },
        );
        return;
    }
    let next = match state.phase {
        Phase::Joining => Stage::Connecting,
        Phase::Reconnecting => Stage::Reconnecting,
        _ if call.ringing && !call.peer_joined => Stage::Ringing,
        _ if call.ring_requested && !call.peer_joined && call.private => Stage::RingRequested,
        Phase::VoiceReady => Stage::Active,
        _ if call.peer_joined => Stage::AnsweredPending,
        _ => Stage::JoinedPending,
    };
    if next == call.stage {
        return;
    }
    if let Some(start) = call.active_since.take() {
        call.elapsed += start.elapsed();
    }
    if next == Stage::Active {
        call.active_since = Some(Instant::now());
    }
    call.stage = next;
    cue(
        state,
        match next {
            Stage::Ringing => Cue::Ring,
            Stage::Active => Cue::Joined,
            _ => Cue::Stop,
        },
    );
}
pub fn gateway_mute(state: &UiState) -> bool {
    state.muted
        || state.deafened
        || state.server_suppressed
        || state.phase != Phase::VoiceReady
        || !state.microphone_permission.usable()
}
pub fn mic_reason(state: &UiState, transmitting: bool) -> &'static str {
    if state.server_suppressed {
        "Server has muted, deafened or suppressed you"
    } else if state.deafened || state.confirmed_deafened == Some(true) {
        "Deafened"
    } else if state.muted {
        "Microphone muted"
    } else if matches!(
        state.microphone_permission,
        crate::microphone::Permission::Denied | crate::microphone::Permission::Restricted
    ) {
        state.microphone_permission.label()
    } else if state.phase == Phase::VoiceWaiting {
        "Unmute pending · encryption is not ready"
    } else if state.phase != Phase::VoiceReady {
        "Microphone closed · voice is not ready"
    } else if state.confirmed_muted != Some(false) || state.confirmed_deafened != Some(false) {
        "Microphone closed · waiting for Discord unmute confirmation"
    } else if state.ptt_enabled && !transmitting {
        "Push-to-talk released"
    } else if !transmitting {
        "Microphone transmission blocked"
    } else {
        "Microphone enabled"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Account, Participant};
    fn state() -> UiState {
        UiState {
            account: Some(Account {
                id: 1,
                name: "Self".into(),
                avatar: None,
            }),
            phase: Phase::Joining,
            selected_channel: Some(10),
            microphone_permission: crate::microphone::Permission::Authorized,
            ..Default::default()
        }
    }
    fn person(id: u64) -> Participant {
        Participant {
            id,
            name: "Member".into(),
            speaking: false,
            muted: true,
            deafened: false,
        }
    }
    // A delayed/partial call update must neither manufacture a ring nor revive
    // a dismissed one; the audio lease must revoke without a UI frame.
    #[test]
    fn incoming_ring_is_scoped_cancellable_and_expires_without_rendering() {
        use serde_json::json;
        use std::sync::atomic::Ordering;
        let mut s = state();
        s.direct_channels.push(crate::social::DirectChannel {
            id: 10,
            name: "Friend".into(),
            last_message_id: None,
            recipients: vec![crate::social::Friend {
                id: 2,
                name: "Friend".into(),
                avatar: None,
            }],
        });
        observe_incoming(
            &mut s,
            "CALL_CREATE",
            &json!({"channel_id":"11","ringing":["1"]}),
        );
        assert!(s.incoming_call.is_none());
        observe_incoming(
            &mut s,
            "CALL_CREATE",
            &json!({"channel_id":"10","ringing":["2"]}),
        );
        assert!(s.incoming_call.is_none());
        let ring = json!({"channel_id":"10","message_id":"100","ringing":["1"]});
        observe_incoming(&mut s, "CALL_CREATE", &ring);
        let generation = s.incoming_call.as_ref().unwrap().generation;
        assert!(incoming_matches(&s, 10, generation));
        assert!(!incoming_matches(&s, 10, generation + 1));
        let lease = s.sound_authority.clone();
        let playing = lease.load(Ordering::Acquire);
        observe_incoming(&mut s, "CALL_UPDATE", &json!({"channel_id":"10"}));
        assert_eq!(s.incoming_call.as_ref().unwrap().generation, generation);
        assert_eq!(clear_incoming(&mut s), Some(generation));
        assert_ne!(lease.load(Ordering::Acquire), playing);
        observe_incoming(&mut s, "CALL_UPDATE", &ring);
        assert!(s.incoming_call.is_none());
        observe_incoming(&mut s, "CALL_CREATE", &ring);
        assert!(s.incoming_call.is_none());
        observe_incoming(
            &mut s,
            "CALL_CREATE",
            &json!({"channel_id":"10","message_id":"101","ringing":["1"]}),
        );
        assert!(s.incoming_call.as_ref().unwrap().generation > generation);
        assert!(!incoming_matches(&s, 10, generation));
        let deadline = s.incoming_call.as_ref().unwrap().received_at + INCOMING_TIMEOUT;
        expire_incoming(&mut s, deadline);
        assert!(s.incoming_call.is_none());
        assert_eq!(s.sound_cue, Cue::Stop);
        observe_incoming(&mut s, "CALL_DELETE", &json!({"channel_id":"10"}));
        assert!(s.dismissed_incoming.is_none());
        observe_incoming(&mut s, "CALL_CREATE", &ring);
        let playing = lease.load(Ordering::Acquire);
        s.deafened = true;
        sync_sound_authority(&s);
        assert_ne!(lease.load(Ordering::Acquire), playing);
    }
    #[test]
    fn observed_call_panel_scopes_ring_answer_pending_active_and_end() {
        let mut s = state();
        start(&mut s, 10, true, "Friend".into());
        refresh(&mut s);
        assert!(!s.current_call.as_ref().unwrap().confirmed);
        server_call(&mut s, 11, Some(true));
        assert!(!s.current_call.as_ref().unwrap().confirmed);
        s.phase = Phase::VoiceWaiting;
        s.participants = vec![person(1)];
        server_call(&mut s, 10, Some(true));
        refresh(&mut s);
        assert_eq!(s.current_call.as_ref().unwrap().stage, Stage::Ringing);
        assert_eq!(s.sound_cue, Cue::Ring);
        assert!(s.current_call.as_ref().unwrap().confirmed);
        assert!(s.current_call.as_ref().unwrap().active_since.is_none());
        s.participants.push(person(2));
        server_call(&mut s, 10, None);
        refresh(&mut s);
        assert_eq!(
            s.current_call.as_ref().unwrap().stage,
            Stage::AnsweredPending
        );
        assert_eq!(s.sound_cue, Cue::Stop);
        s.phase = Phase::VoiceReady;
        refresh(&mut s);
        assert_eq!(s.current_call.as_ref().unwrap().stage, Stage::Active);
        assert!(s.current_call.as_ref().unwrap().active_since.is_some());
        s.phase = Phase::Reconnecting;
        refresh(&mut s);
        assert!(s.current_call.as_ref().unwrap().active_since.is_none());
        s.phase = Phase::SignalingReady;
        s.selected_channel = None;
        refresh(&mut s);
        assert!(s.current_call.is_none());
        assert!(s.messages.is_empty());
    }
    #[test]
    fn unmute_intent_is_pending_until_devices_and_encryption_are_ready() {
        let mut s = state();
        s.muted = false;
        s.phase = Phase::VoiceWaiting;
        assert!(gateway_mute(&s));
        assert!(mic_reason(&s, false).contains("pending"));
        s.phase = Phase::VoiceReady;
        assert!(!gateway_mute(&s));
        s.confirmed_muted = Some(true);
        assert!(mic_reason(&s, false).contains("confirmation"));
        s.server_suppressed = true;
        assert!(gateway_mute(&s));
        s.server_suppressed = false;
        s.microphone_permission = crate::microphone::Permission::Denied;
        assert!(gateway_mute(&s));
        assert!(mic_reason(&s, false).contains("denied"));
    }
}
