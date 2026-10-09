use crate::id::{ChannelId, GuildId, UserId};
use std::fmt;

#[cfg(feature = "gateway")]
#[derive(Clone, Debug)]
pub(crate) enum ConnectionProgress {
    Complete(ConnectionInfo),
    Incomplete(Partial),
}

#[cfg(feature = "gateway")]
impl ConnectionProgress {
    pub(crate) fn new(guild_id: GuildId, user_id: UserId, channel_id: ChannelId) -> Self {
        ConnectionProgress::Incomplete(Partial {
            channel_id,
            guild_id,
            user_id,
            token: None,
            endpoint: None,
            session_id: None,
        })
    }

    pub(crate) fn get_connection_info(&self) -> Option<&ConnectionInfo> {
        if let Self::Complete(c) = self {
            Some(c)
        } else {
            None
        }
    }

    pub(crate) fn in_progress(&self) -> bool {
        matches!(self, ConnectionProgress::Incomplete(_))
    }

    pub(crate) fn channel_id(&self) -> ChannelId {
        match self {
            ConnectionProgress::Complete(conn_info) => conn_info.channel_id,
            ConnectionProgress::Incomplete(part) => part.channel_id,
        }
    }

    pub(crate) fn guild_id(&self) -> GuildId {
        match self {
            // The gateway frontend only constructs guild calls. Private calls
            // are supplied directly to Driver and never enter this state.
            ConnectionProgress::Complete(conn_info) => conn_info
                .guild_id
                .expect("gateway-managed voice calls always have a guild"),
            ConnectionProgress::Incomplete(part) => part.guild_id,
        }
    }

    pub(crate) fn user_id(&self) -> UserId {
        match self {
            ConnectionProgress::Complete(conn_info) => conn_info.user_id,
            ConnectionProgress::Incomplete(part) => part.user_id,
        }
    }

    pub(crate) fn info(&self) -> Option<ConnectionInfo> {
        self.get_connection_info().cloned()
    }

    pub(crate) fn apply_state_update(&mut self, session_id: String, channel_id: ChannelId) -> bool {
        if self.channel_id() != channel_id {
            // Likely that the bot was moved to a different channel by an admin.
            *self = ConnectionProgress::new(self.guild_id(), self.user_id(), channel_id);
        }

        match self {
            Self::Complete(c) => {
                let should_reconn = c.session_id != session_id;
                c.session_id = session_id;
                should_reconn
            }
            Self::Incomplete(i) => i
                .apply_state_update(session_id, channel_id)
                .map(|info| {
                    *self = Self::Complete(info);
                })
                .is_some(),
        }
    }

    pub(crate) fn apply_server_update(&mut self, endpoint: String, token: String) -> bool {
        match self {
            Self::Complete(c) => {
                let should_reconn = c.endpoint != endpoint || c.token != token;

                c.endpoint = endpoint;
                c.token = token;

                should_reconn
            }
            Self::Incomplete(i) => i
                .apply_server_update(endpoint, token)
                .map(|info| {
                    *self = Self::Complete(info);
                })
                .is_some(),
        }
    }
}

/// Parameters and information needed to start communicating with Discord's voice servers, either
/// with the Songbird driver, lavalink, or other system.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct ConnectionInfo {
    /// ID of the voice channel being joined.
    pub channel_id: ChannelId,
    /// URL of the voice websocket gateway server assigned to this call.
    pub endpoint: String,
    /// ID of the target voice channel's parent guild.
    ///
    /// None for a private call supplied to the standalone driver. The bot
    /// gateway frontend continues to create guild calls only.
    pub guild_id: Option<GuildId>,
    /// Unique string describing this session for validation/authentication purposes.
    pub session_id: String,
    /// Ephemeral secret used to validate the above session.
    pub token: String,
    /// UserID of this bot.
    pub user_id: UserId,
}

#[cfg(feature = "driver")]
impl ConnectionInfo {
    /// Voice Identify/Resume server ID: guild for server channels, channel for
    /// private calls. The voice wire model names this numeric type GuildId.
    pub fn server_id(&self) -> crate::model::id::GuildId {
        crate::model::id::GuildId(
            self.guild_id
                .map_or(self.channel_id.0.get(), |id| id.0.get()),
        )
    }
}

impl fmt::Debug for ConnectionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionInfo")
            .field("channel_id", &self.channel_id)
            .field("endpoint", &self.endpoint)
            .field("guild_id", &self.guild_id)
            .field("session_id", &"<secret>")
            .field("token", &"<secret>")
            .field("user_id", &self.user_id)
            .finish()
    }
}

#[cfg(feature = "gateway")]
#[derive(Clone)]
pub(crate) struct Partial {
    pub channel_id: ChannelId,
    pub endpoint: Option<String>,
    pub guild_id: GuildId,
    pub session_id: Option<String>,
    pub token: Option<String>,
    pub user_id: UserId,
}

#[cfg(feature = "gateway")]
impl fmt::Debug for Partial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Partial")
            .field("channel_id", &self.channel_id)
            .field("endpoint", &self.endpoint)
            .field("guild_id", &self.guild_id)
            .field("session_id", &"<secret>")
            .field("token_is_some", &self.token.is_some())
            .field("user_id", &self.user_id)
            .finish()
    }
}

#[cfg(feature = "gateway")]
impl Partial {
    fn finalise(&mut self) -> Option<ConnectionInfo> {
        if self.endpoint.is_some() && self.session_id.is_some() && self.token.is_some() {
            let endpoint = self.endpoint.take().unwrap();
            let session_id = self.session_id.take().unwrap();
            let token = self.token.take().unwrap();

            Some(ConnectionInfo {
                channel_id: self.channel_id,
                endpoint,
                session_id,
                token,
                guild_id: Some(self.guild_id),
                user_id: self.user_id,
            })
        } else {
            None
        }
    }

    fn apply_state_update(
        &mut self,
        session_id: String,
        channel_id: ChannelId,
    ) -> Option<ConnectionInfo> {
        if self.channel_id != channel_id {
            self.endpoint = None;
            self.token = None;
        }

        self.channel_id = channel_id;
        self.session_id = Some(session_id);

        self.finalise()
    }

    fn apply_server_update(&mut self, endpoint: String, token: String) -> Option<ConnectionInfo> {
        self.endpoint = Some(endpoint);
        self.token = Some(token);

        self.finalise()
    }
}
