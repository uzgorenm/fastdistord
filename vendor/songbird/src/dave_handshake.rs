//! Bounded structural DAVE observations. No wire payloads or identifiers enter this type.
use std::{collections::VecDeque, sync::Mutex, time::Instant};

/// Locally defined handshake events; numeric values are versions, counts or transition IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaveStage {
    /// Secure WebSocket connected.
    Connected,
    /// Identify sent.
    Identify,
    /// Session description negotiated a DAVE version.
    Protocol,
    /// Key package sent (no package bytes retained).
    KeyPackage,
    /// External sender accepted.
    ExternalSender,
    /// Proposals received.
    Proposals,
    /// Proposals processed without a candidate commit.
    ProposalsPending,
    /// Proposals ignored because no local session exists.
    ProposalsWithoutSession,
    /// Commit and optional welcome sent.
    CommitSent,
    /// Commit accepted locally.
    CommitAccepted,
    /// Welcome accepted locally.
    WelcomeAccepted,
    /// Transition acknowledgement sent.
    TransitionAck,
    /// Transition executed.
    TransitionExecuted,
    /// A new epoch was announced.
    Epoch,
    /// MLS processing failed (no error payload retained).
    MlsFailed,
    /// Known voice participant count changed.
    Peers,
    /// Negotiated media readiness changed.
    MediaReady,
}
#[derive(Debug)]
struct State {
    start: Instant,
    enabled: bool,
    trace: VecDeque<(u128, DaveStage, u16)>,
    protocol: u16,
    peers: usize,
    roster: Option<usize>,
    external: bool,
    proposals: bool,
    commit_sent: bool,
    prepared: bool,
    ready: bool,
    failed: bool,
}
/// Shared, memory-only structural handshake state and opt-in bounded trace.
#[derive(Debug)]
pub struct DaveHandshake(Mutex<State>);
impl Default for DaveHandshake {
    fn default() -> Self {
        Self(Mutex::new(State {
            start: Instant::now(),
            enabled: false,
            trace: VecDeque::new(),
            protocol: 0,
            peers: 1,
            roster: None,
            external: false,
            proposals: false,
            commit_sent: false,
            prepared: false,
            ready: false,
            failed: false,
        }))
    }
}
impl DaveHandshake {
    /// Reset a join's observations while retaining the user's trace preference.
    pub fn begin(&self) {
        if let Ok(mut s) = self.0.lock() {
            let enabled = s.enabled;
            let roster = s.roster;
            *s = State {
                start: Instant::now(),
                enabled,
                trace: VecDeque::new(),
                protocol: 0,
                peers: 1,
                roster,
                external: false,
                proposals: false,
                commit_sent: false,
                prepared: false,
                ready: false,
                failed: false,
            };
        }
    }
    /// Authoritative account Gateway voice roster count, or unknown when incomplete.
    pub fn set_roster_count(&self, count: Option<usize>) {
        if let Ok(mut s) = self.0.lock() {
            s.roster = count;
        }
    }
    /// Enable or clear structural tracing. Nothing is written to disk.
    pub fn set_enabled(&self, enabled: bool) {
        if let Ok(mut s) = self.0.lock() {
            s.enabled = enabled;
            if !enabled {
                s.trace.clear();
            }
        }
    }
    /// Whether the user has enabled trace retention.
    pub fn enabled(&self) -> bool {
        self.0.lock().is_ok_and(|s| s.enabled)
    }
    /// Observe only a locally typed stage and a bounded numeric value.
    pub fn record(&self, stage: DaveStage, value: u16) {
        if let Ok(mut s) = self.0.lock() {
            if stage == DaveStage::MediaReady && s.ready == (value != 0) {
                return;
            }
            match stage {
                DaveStage::Protocol => s.protocol = value,
                DaveStage::Epoch => {
                    s.protocol = value;
                    s.proposals = false;
                    s.commit_sent = false;
                    s.prepared = false;
                    s.ready = false;
                    s.failed = false;
                }
                DaveStage::Peers => s.peers = usize::from(value),
                DaveStage::ExternalSender => s.external = true,
                DaveStage::Proposals => s.proposals = true,
                DaveStage::CommitSent => s.commit_sent = true,
                DaveStage::CommitAccepted | DaveStage::WelcomeAccepted => {
                    s.prepared = true;
                    s.failed = false;
                }
                DaveStage::MlsFailed => s.failed = true,
                DaveStage::MediaReady => s.ready = value != 0,
                _ => {}
            }
            if s.enabled {
                let elapsed = s.start.elapsed().as_millis();
                if s.trace.len() == 64 {
                    s.trace.pop_front();
                }
                s.trace.push_back((elapsed, stage, value));
            }
        }
    }
    /// Stay joined without media only while no other known participant needs an MLS exchange.
    pub fn idle_without_peer(&self) -> bool {
        self.0
            .lock()
            .is_ok_and(|s| s.roster == Some(1) && s.peers <= 1 && !s.failed)
    }
    /// Whether cryptographic processing has failed since the last successful group preparation.
    pub fn failed(&self) -> bool {
        self.0.lock().map_or(true, |s| s.failed)
    }
    /// Safe explanation of the missing stage; never includes server text.
    pub fn summary(&self) -> String {
        let Ok(s) = self.0.lock() else {
            return "Handshake state unavailable".into();
        };
        let stage = if s.failed {
            "MLS processing failed"
        } else if s.ready {
            "Encrypted media ready"
        } else if s.protocol == 0 {
            "Waiting for server DAVE negotiation"
        } else if s.prepared {
            "Waiting for transition execution"
        } else if !s.external {
            "Waiting for external sender"
        } else if s.commit_sent {
            "Waiting for server acceptance of MLS commit or welcome"
        } else if s.proposals {
            "MLS proposals received; waiting for commit or welcome"
        } else {
            "Waiting for MLS proposals or welcome"
        };
        let roster = s.roster.map_or_else(|| "unknown".into(), |n| n.to_string());
        format!(
            "{stage} · protocol {} · voice peers {} · account roster {roster}",
            s.protocol, s.peers
        )
    }
    /// Export only the last 64 structural events for the user's explicit copy action.
    pub fn trace(&self) -> String {
        self.0
            .lock()
            .map(|s| {
                s.trace
                    .iter()
                    .map(|(ms, stage, value)| format!("{ms}ms {stage:?} {value}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_authoritative_solo_roster_allows_idle_without_media() {
        let state = DaveHandshake::default();
        assert!(!state.idle_without_peer());
        state.set_roster_count(Some(1));
        assert!(state.idle_without_peer());
        state.record(DaveStage::Peers, 2);
        assert!(!state.idle_without_peer());
        state.record(DaveStage::Peers, 1);
        state.set_roster_count(Some(2));
        assert!(!state.idle_without_peer());
        state.set_roster_count(Some(1));
        state.record(DaveStage::MlsFailed, 30);
        assert!(!state.idle_without_peer());
        assert!(state.failed());
    }
    #[test]
    fn distinguishes_missing_proposals_from_unaccepted_commit() {
        let state = DaveHandshake::default();
        state.record(DaveStage::Protocol, 1);
        state.record(DaveStage::ExternalSender, 25);
        assert!(state.summary().starts_with("Waiting for MLS proposals"));
        state.record(DaveStage::Proposals, 27);
        assert!(state.summary().starts_with("MLS proposals received"));
        state.record(DaveStage::CommitSent, 28);
        assert!(state.summary().starts_with("Waiting for server acceptance"));
        state.record(DaveStage::Epoch, 1);
        assert!(state.summary().starts_with("Waiting for MLS proposals"));
        assert!(!state.idle_without_peer());
    }
    #[test]
    fn trace_is_opt_in_bounded_and_join_scoped() {
        let state = DaveHandshake::default();
        state.record(DaveStage::Protocol, 1);
        assert!(state.trace().is_empty());
        state.set_enabled(true);
        for value in 0..100 {
            state.record(DaveStage::Protocol, value);
        }
        assert_eq!(state.trace().lines().count(), 64);
        state.begin();
        assert!(state.enabled() && state.trace().is_empty());
        state.record(DaveStage::Connected, 0);
        state.set_enabled(false);
        assert!(state.trace().is_empty());
    }
}
