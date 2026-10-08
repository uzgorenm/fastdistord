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
#[derive(Clone, Debug)]
pub struct Event {
    pub channel: u64,
    pub text: &'static str,
    pub at: u64,
}
pub fn start(state: &mut UiState, channel: u64, private: bool, target: String) {
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
pub fn event(state: &mut UiState, channel: u64, text: &'static str) {
    if state.call_events.len() == 32 {
        state.call_events.remove(0);
    }
    state.call_events.push(Event {
        channel,
        text,
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64),
    });
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
    let first = !call.confirmed;
    let answered = peer && !call.peer_joined;
    call.confirmed = true;
    call.peer_joined = peer;
    if let Some(ringing) = ringing {
        call.ringing = ringing;
    }
    if peer {
        call.ringing = false;
    }
    if first {
        event(state, channel, "You started a call");
    }
    if answered {
        event(state, channel, "Call answered");
    }
}
pub fn end(state: &mut UiState, reason: &'static str) {
    if let Some(call) = state.current_call.take() {
        if call.private {
            event(
                state,
                call.channel,
                if call.confirmed {
                    reason
                } else {
                    "Call canceled"
                },
            );
        }
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
    #[test]
    fn real_call_events_scope_ring_answer_pending_active_and_end() {
        let mut s = state();
        start(&mut s, 10, true, "Friend".into());
        refresh(&mut s);
        assert!(s.call_events.is_empty());
        server_call(&mut s, 11, Some(true));
        assert!(s.call_events.is_empty());
        s.phase = Phase::VoiceWaiting;
        s.participants = vec![person(1)];
        server_call(&mut s, 10, Some(true));
        refresh(&mut s);
        assert_eq!(s.current_call.as_ref().unwrap().stage, Stage::Ringing);
        assert_eq!(s.sound_cue, Cue::Ring);
        assert_eq!(s.call_events[0].text, "You started a call");
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
        assert_eq!(s.call_events.last().unwrap().text, "Call ended");
        assert!(s.call_events.iter().all(|e| e.channel == 10));
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
