//! Bounded account read-state tracking. Missing snapshots and interrupted event
//! streams remain unknown; selecting/fetching a channel never acknowledges it.
use crate::account::snowflake;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const MAX_CHANNELS: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityBadge {
    pub unread: Option<bool>,
    /// The last authoritative count while subsequent events provably preserve it.
    /// None means unknown, never zero. No guessed role/muted/blocked-user count.
    pub mentions: Option<u32>,
    pub counts_complete: bool,
}

#[derive(Clone, Debug, Default)]
struct ChannelActivity {
    ack: Option<u64>,
    latest: Option<u64>,
    latest_known: bool,
    continuous: bool,
    mentions: Option<u32>,
    touched: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ActivityState {
    channels: BTreeMap<u64, ChannelActivity>,
    clock: u64,
}

impl ActivityState {
    pub fn badge(&self, channel: u64) -> ActivityBadge {
        let Some(state) = self.channels.get(&channel) else {
            return ActivityBadge::default();
        };
        let unread = match (state.ack, state.latest) {
            (Some(ack), Some(latest)) if latest > ack => Some(true),
            (Some(_), _) if state.latest_known && state.continuous => Some(false),
            _ => None,
        };
        ActivityBadge {
            unread,
            mentions: state.mentions,
            counts_complete: state.mentions.is_some(),
        }
    }

    fn channel(&mut self, id: u64) -> &mut ChannelActivity {
        self.clock = self.clock.wrapping_add(1);
        if !self.channels.contains_key(&id)
            && self.channels.len() >= MAX_CHANNELS
            && let Some(old) = self
                .channels
                .iter()
                .min_by_key(|(_, s)| s.touched)
                .map(|(id, _)| *id)
        {
            self.channels.remove(&old);
        }
        let state = self.channels.entry(id).or_default();
        state.touched = self.clock;
        state
    }

    /// A missing/unresumable gateway interval invalidates completeness, not known unread evidence.
    pub fn mark_gap(&mut self) {
        for state in self.channels.values_mut() {
            state.continuous = false;
            state.mentions = None;
        }
    }

    pub fn apply_dispatch(&mut self, kind: &str, data: &Value, own: Option<u64>) {
        match kind {
            "READ_STATE_SNAPSHOT" => {
                self.channels.clear();
                if let Some(states) = data["read_states"].as_array() {
                    for value in states.iter().take(MAX_CHANNELS) {
                        let Some(id) = snowflake(&value["id"]) else {
                            continue;
                        };
                        let state = self.channel(id);
                        state.ack = zero_or_snowflake(&value["last_message_id"]);
                        state.mentions = value["mention_count"]
                            .as_u64()
                            .and_then(|v| u32::try_from(v).ok());
                        state.continuous = true;
                    }
                }
                if let Some(channels) = data["channels"].as_array() {
                    for value in channels.iter().take(MAX_CHANNELS) {
                        self.observe_channel(value);
                    }
                }
            }
            "CHANNEL_CREATE" | "CHANNEL_UPDATE" => self.observe_channel(data),
            "CHANNEL_DELETE" => {
                if let Some(id) = snowflake(&data["id"]) {
                    self.channels.remove(&id);
                }
            }
            "MESSAGE_ACK" => {
                let (Some(channel), Some(message)) = (
                    snowflake(&data["channel_id"]),
                    zero_or_snowflake(&data["message_id"]),
                ) else {
                    return;
                };
                let state = self.channel(channel);
                // A manual ACK can intentionally move the read marker backwards.
                state.ack = Some(message);
                state.mentions = data["mention_count"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok());
            }
            "MESSAGE_CREATE" => {
                let (Some(channel), Some(id)) =
                    (snowflake(&data["channel_id"]), snowflake(&data["id"]))
                else {
                    return;
                };
                // An outgoing message is not a new unread item; it also must not
                // silently erase any pre-existing unread incoming messages.
                if own.is_some() && own == snowflake(&data["author"]["id"]) {
                    return;
                }
                let state = self.channel(channel);
                // A replay or event older than the snapshot cannot increment twice.
                let newer = state.latest.is_none_or(|latest| id > latest);
                state.latest = Some(state.latest.unwrap_or(0).max(id));
                if !newer || state.ack.is_some_and(|ack| id <= ack) {
                    return;
                }
                let mention = direct_mention(data, own);
                // Live role/everyone mentions and missing membership/preferences do
                // not have enough evidence for a faithful account badge count.
                if mention == Some(true) || mention.is_none() {
                    state.mentions = None;
                }
            }
            "MESSAGE_UPDATE" => {
                let (Some(channel), Some(id)) =
                    (snowflake(&data["channel_id"]), snowflake(&data["id"]))
                else {
                    return;
                };
                let state = self.channel(channel);
                if state.ack.is_none_or(|ack| id > ack)
                    && ["mentions", "mention_roles", "mention_everyone"]
                        .iter()
                        .any(|key| data.get(key).is_some())
                {
                    state.mentions = None;
                }
            }
            "MESSAGE_DELETE" => {
                if let (Some(channel), Some(id)) =
                    (snowflake(&data["channel_id"]), snowflake(&data["id"]))
                {
                    self.delete_message(channel, id);
                }
            }
            "MESSAGE_DELETE_BULK" => {
                if let (Some(channel), Some(ids)) =
                    (snowflake(&data["channel_id"]), data["ids"].as_array())
                {
                    for id in ids.iter().take(100).filter_map(snowflake) {
                        self.delete_message(channel, id);
                    }
                }
            }
            _ => {}
        }
    }

    fn observe_channel(&mut self, data: &Value) {
        let Some(id) = snowflake(&data["id"]) else {
            return;
        };
        let Some(latest) = data.get("last_message_id") else {
            return;
        };
        let state = self.channel(id);
        if latest.is_null() {
            state.latest_known = true;
            state.latest = None;
        } else if let Some(id) = snowflake(latest) {
            state.latest_known = true;
            state.latest = Some(state.latest.unwrap_or(0).max(id));
        }
    }

    fn delete_message(&mut self, channel: u64, id: u64) {
        let state = self.channel(channel);
        if state.ack.is_none_or(|ack| id > ack) {
            state.mentions = None;
        }
        if state.latest == Some(id) {
            // The previous message might be outside this bounded event window.
            state.latest = None;
            state.latest_known = false;
        }
    }
}

fn zero_or_snowflake(value: &Value) -> Option<u64> {
    if value == "0" || value == &json!(0) {
        Some(0)
    } else {
        snowflake(value)
    }
}

fn direct_mention(data: &Value, own: Option<u64>) -> Option<bool> {
    let own = own?;
    let mentions = data["mentions"].as_array().filter(|v| v.len() <= 100)?;
    let roles = data["mention_roles"]
        .as_array()
        .filter(|v| v.len() <= 100)?;
    let everyone = data["mention_everyone"].as_bool()?;
    if everyone || !roles.is_empty() {
        return None;
    }
    Some(
        mentions
            .iter()
            .any(|user| snowflake(&user["id"]) == Some(own)),
    )
}

/// Copy only bounded IDs and counts from READY; never forward session/account data.
pub(crate) fn ready_snapshot(data: &Value) -> Value {
    let states = data["read_state"]["entries"]
        .as_array()
        .or_else(|| data["read_state"].as_array());
    let read_states: Vec<_> = states
        .into_iter()
        .flatten()
        .filter(|v| v["read_state_type"].as_u64().unwrap_or(0) == 0)
        .take(MAX_CHANNELS)
        .filter_map(|v| {
            let id = snowflake(&v["id"])?;
            let ack = zero_or_snowflake(v.get("last_acked_id").unwrap_or(&v["last_message_id"]));
            let count = v
                .get("badge_count")
                .unwrap_or(&v["mention_count"])
                .as_u64()
                .and_then(|n| u32::try_from(n).ok());
            Some(json!({"id": id, "last_message_id": ack, "mention_count": count}))
        })
        .collect();
    let mut channels = Vec::new();
    let private = data["private_channels"].as_array().into_iter().flatten();
    let guild = data["guilds"]
        .as_array()
        .into_iter()
        .flatten()
        .take(250)
        .flat_map(|g| g["channels"].as_array().into_iter().flatten());
    for channel in private.chain(guild).take(MAX_CHANNELS) {
        let Some(id) = snowflake(&channel["id"]) else {
            continue;
        };
        let mut entry = json!({"id": id});
        if let Some(latest) = channel.get("last_message_id") {
            if latest.is_null() {
                entry["last_message_id"] = Value::Null;
            } else if let Some(id) = snowflake(latest) {
                entry["last_message_id"] = json!(id);
            }
        }
        channels.push(entry);
    }
    json!({"read_states": read_states, "channels": channels})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Value {
        json!({"read_states":[{"id":"10","last_message_id":"20","mention_count":2}],"channels":[{"id":"10","last_message_id":"30"}]})
    }
    #[test]
    fn missing_data_and_gaps_never_become_read_or_zero_mentions() {
        let mut state = ActivityState::default();
        assert_eq!(state.badge(10), ActivityBadge::default());
        state.apply_dispatch("READ_STATE_SNAPSHOT", &snapshot(), Some(1));
        assert_eq!(state.badge(10).unread, Some(true));
        assert_eq!(state.badge(10).mentions, Some(2));
        state.mark_gap();
        assert_eq!(state.badge(10).unread, Some(true));
        assert_eq!(state.badge(10).mentions, None);
        state.apply_dispatch(
            "MESSAGE_ACK",
            &json!({"channel_id":"10","message_id":"30"}),
            Some(1),
        );
        assert_eq!(state.badge(10).unread, None);
        assert_eq!(state.badge(10).mentions, None);
    }
    #[test]
    fn ack_race_keeps_newer_message_unread_and_own_send_never_acknowledges() {
        let mut state = ActivityState::default();
        state.apply_dispatch("READ_STATE_SNAPSHOT", &snapshot(), Some(1));
        let created = json!({"channel_id":"10","id":"40","author":{"id":"2"},"mentions":[],"mention_roles":[],"mention_everyone":false});
        state.apply_dispatch("MESSAGE_CREATE", &created, Some(1));
        state.apply_dispatch(
            "MESSAGE_CREATE",
            &json!({"channel_id":"10","id":"45","author":{"id":"1"}}),
            Some(1),
        );
        state.apply_dispatch(
            "MESSAGE_ACK",
            &json!({"channel_id":"10","message_id":"30"}),
            Some(1),
        );
        assert_eq!(state.badge(10).unread, Some(true));
        assert_eq!(state.badge(10).mentions, None);
        state.apply_dispatch(
            "MESSAGE_ACK",
            &json!({"channel_id":"10","message_id":"40","mention_count":0}),
            Some(1),
        );
        assert_eq!(state.badge(10).unread, Some(false));
        assert_eq!(state.badge(10).mentions, Some(0));
        state.apply_dispatch(
            "MESSAGE_CREATE",
            &json!({"channel_id":"10","id":"50","author":{"id":"1"}}),
            Some(1),
        );
        assert_eq!(state.badge(10).unread, Some(false));
        state.apply_dispatch(
            "MESSAGE_ACK",
            &json!({"channel_id":"10","message_id":"20","mention_count":3,"manual":true}),
            Some(1),
        );
        assert_eq!(state.badge(10).unread, Some(true));
        assert_eq!(state.badge(10).mentions, Some(3));
        // REST completion is not a read-state event. In particular, an older
        // request completing after this manual ACK cannot restore its old marker.
        let authoritative = state.badge(10);
        state.apply_dispatch(
            "REST_ACK_COMPLETED",
            &json!({"channel_id":"10","message_id":"40"}),
            Some(1),
        );
        assert_eq!(state.badge(10), authoritative);
    }
    #[test]
    fn deletes_updates_and_live_mentions_do_not_invent_exact_badge_counts() {
        let mut state = ActivityState::default();
        state.apply_dispatch("READ_STATE_SNAPSHOT", &snapshot(), Some(1));
        state.apply_dispatch(
            "MESSAGE_UPDATE",
            &json!({"channel_id":"10","id":"30","mentions":[{"id":"1"}]}),
            Some(1),
        );
        assert_eq!(state.badge(10).mentions, None);
        state.apply_dispatch(
            "MESSAGE_DELETE",
            &json!({"channel_id":"10","id":"30"}),
            Some(1),
        );
        assert_eq!(state.badge(10).unread, None);
        state.apply_dispatch("MESSAGE_CREATE", &json!({"channel_id":"10","id":"40","mentions":[{"id":"1"}],"mention_roles":[],"mention_everyone":false}), Some(1));
        assert_eq!(state.badge(10).unread, Some(true));
        assert_eq!(state.badge(10).mentions, None);
    }
    #[test]
    fn ready_projection_and_state_are_bounded_and_exclude_session_data() {
        let ready = json!({"session_id":"secret","read_state":{"entries":[{"id":"10","last_message_id":"0","mention_count":0},{"id":"11","read_state_type":1,"last_message_id":"20"}]},"private_channels":[{"id":"10","last_message_id":null},{"id":"12"}]});
        let projection = ready_snapshot(&ready);
        assert!(projection.get("session_id").is_none());
        let mut state = ActivityState::default();
        state.apply_dispatch("READ_STATE_SNAPSHOT", &projection, None);
        assert_eq!(state.badge(10).unread, Some(false));
        assert_eq!(state.badge(11).unread, None);
        assert_eq!(state.badge(12).unread, None);
        for id in 1..=2000 {
            state.apply_dispatch("MESSAGE_CREATE", &json!({"channel_id":id,"id":id}), None);
        }
        assert_eq!(state.channels.len(), MAX_CHANNELS);
    }
}
