//! Socket-free access to the production receiver for offline integration tests.
#![allow(missing_docs)]

use super::*;
use crate::{driver::tasks::error::Result, events::context_data::VoiceData};
use serenity_voice_model::id::UserId;

pub struct ReceiveHarness {
    state: UdpRx,
    interconnect: Interconnect,
}

impl ReceiveHarness {
    pub fn new(mode: CryptoMode, key: &[u8], config: Config, session: davey::DaveSession) -> Self {
        let (core, _) = flume::unbounded();
        let (events, _) = flume::unbounded();
        let (mixer, _) = flume::unbounded();
        let (_, rx) = flume::unbounded();
        Self {
            state: UdpRx {
                cipher: mode.cipher_from_key(key).expect("fixture transport key"),
                crypto_mode: mode,
                decoder_map: HashMap::new(),
                config,
                rx,
                ssrc_signalling: Arc::new(SsrcTracker::default()),
                dave_session: Arc::new(RwLock::new(Some(session))),
                dave_protocol_version: Arc::new(AtomicU16::new(davey::DAVE_PROTOCOL_VERSION)),
            },
            interconnect: Interconnect {
                core,
                events,
                mixer,
            },
        }
    }

    pub fn map_ssrc(&self, ssrc: u32, user: u64) {
        let tracker = &self.state.ssrc_signalling;
        tracker.ssrc_user_map.insert(ssrc, UserId(user));
        tracker.user_ssrc_map.insert(UserId(user), ssrc);
    }

    pub async fn receive(&mut self, packet: &[u8]) {
        self.state
            .process_udp_message(&self.interconnect, BytesMut::from(packet))
            .await;
    }

    pub fn voice_tick(&mut self, ssrc: u32) -> Result<Option<VoiceData>> {
        match self.state.decoder_map.get_mut(&ssrc) {
            Some(state) => state.get_voice_tick(&self.state.config),
            None => Ok(None),
        }
    }
}
