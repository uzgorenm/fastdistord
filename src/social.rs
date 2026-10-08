//! Bounded friends and private conversations. Parsing never performs account actions.
//! Personal-account relationships/calls use reverse-engineered Discord protocols;
//! see https://docs.discord.food/resources/relationships and /resources/channel.
use crate::account::{display_name, snowflake};
use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::HashSet;

const MAX_SOCIAL: usize = 1000;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Friend {
    pub id: u64,
    pub avatar: Option<String>,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectChannel {
    pub id: u64,
    pub name: String,
    pub recipients: Vec<Friend>,
    pub last_message_id: Option<u64>,
}
pub struct DirectChannelsSnapshot {
    pub channels: Vec<DirectChannel>,
    pub skipped: usize,
}
pub fn parse_friends(value: &Value) -> Result<Vec<Friend>> {
    let Some(values) = value.as_array().filter(|v| v.len() <= MAX_SOCIAL) else {
        bail!("Discord returned an invalid or oversized friends list.");
    };
    let mut seen = HashSet::new();
    let mut friends = Vec::new();
    for relationship in values {
        // Blocked users and pending requests are never presented as friends.
        if relationship["type"].as_u64() != Some(1) {
            continue;
        }
        let Some(id) = snowflake(&relationship["user"]["id"]) else {
            continue;
        };
        if relationship
            .get("id")
            .is_some_and(|v| snowflake(v) != Some(id))
        {
            continue;
        }
        if seen.insert(id) {
            friends.push(Friend {
                id,
                avatar: crate::profiles::hash(&relationship["user"]["avatar"]),
                name: display_name(&relationship["user"]),
            });
        }
    }
    friends.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Ok(friends)
}
pub fn parse_direct_channels(value: &Value) -> Result<DirectChannelsSnapshot> {
    let Some(values) = value.as_array().filter(|v| v.len() <= MAX_SOCIAL) else {
        bail!("Discord returned invalid or oversized conversations.");
    };
    let mut seen = HashSet::new();
    let mut channels = Vec::new();
    let mut skipped = 0;
    for value in values {
        // List snapshots may contain partial/unsupported channels. No recipient
        // IDs are guessed or filled from relationships: only validated objects
        // enter the send/call scope. Explicit open-DM responses stay strict.
        let Ok(channel) = parse_direct_channel(value) else {
            skipped += 1;
            continue;
        };
        if seen.insert(channel.id) {
            channels.push(channel);
        }
    }
    Ok(DirectChannelsSnapshot { channels, skipped })
}
pub fn parse_direct_channel(value: &Value) -> Result<DirectChannel> {
    if !matches!(value["type"].as_u64(), Some(1 | 3))
        || value.get("guild_id").is_some_and(|v| !v.is_null())
    {
        bail!("Discord returned a non-private conversation.");
    }
    let Some(id) = snowflake(&value["id"]) else {
        bail!("Discord returned an invalid conversation ID.");
    };
    let Some(users) = value["recipients"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 25)
    else {
        bail!("Discord returned invalid conversation recipients.");
    };
    if value["type"].as_u64() == Some(1) && users.len() != 1 {
        bail!("Discord returned an invalid direct conversation.");
    }
    let mut seen = HashSet::new();
    let mut recipients = Vec::new();
    for user in users {
        let Some(id) = snowflake(&user["id"]) else {
            bail!("Discord returned an invalid recipient ID.");
        };
        if !seen.insert(id) {
            bail!("Discord returned duplicate conversation recipients.");
        }
        recipients.push(Friend {
            id,
            avatar: crate::profiles::hash(&user["avatar"]),
            name: display_name(user),
        });
    }
    let name = value["name"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.chars().take(100).collect())
        .unwrap_or_else(|| {
            recipients
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
                .chars()
                .take(100)
                .collect()
        });
    Ok(DirectChannel {
        id,
        name,
        recipients,
        last_message_id: snowflake(&value["last_message_id"]),
    })
}
/// Latest DM Snowflake provides chronology without fetching conversation history.
pub fn sort_by_activity(friends: &mut [Friend], channels: &[DirectChannel]) {
    let recent: std::collections::HashMap<_, _> = channels
        .iter()
        .filter(|c| c.recipients.len() == 1)
        .filter_map(|c| c.last_message_id.map(|m| (c.recipients[0].id, m)))
        .fold(
            std::collections::HashMap::<u64, u64>::new(),
            |mut map, (id, m)| {
                map.entry(id)
                    .and_modify(|old| *old = (*old).max(m))
                    .or_insert(m);
                map
            },
        );
    friends.sort_by(|a, b| {
        recent
            .get(&b.id)
            .cmp(&recent.get(&a.id))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then(a.id.cmp(&b.id))
    });
}
pub fn observe_message(channels: &mut [DirectChannel], channel: u64, message: u64) {
    if let Some(dm) = channels.iter_mut().find(|c| c.id == channel) {
        dm.last_message_id = Some(dm.last_message_id.unwrap_or(0).max(message));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn friend_order_tracks_newest_known_dm_without_history_requests() {
        let mut friends = parse_friends(&json!([
            {"type":1,"user":{"id":"1","username":"Zed"}},
            {"type":1,"user":{"id":"2","username":"Amy"}},
            {"type":1,"user":{"id":"3","username":"Bob"}}
        ]))
        .unwrap();
        let mut channels = parse_direct_channels(&json!([
            {"id":"10","type":1,"recipients":[{"id":"1"}],"last_message_id":"500"},
            {"id":"20","type":1,"recipients":[{"id":"2"}],"last_message_id":"400"}
        ]))
        .unwrap()
        .channels;
        sort_by_activity(&mut friends, &channels);
        assert_eq!(friends.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2, 3]);
        observe_message(&mut channels, 20, 600);
        observe_message(&mut channels, 20, 300); // Stale replay cannot move activity backwards.
        sort_by_activity(&mut friends, &channels);
        assert_eq!(friends.iter().map(|f| f.id).collect::<Vec<_>>(), [2, 1, 3]);
    }
    #[test]
    fn blocked_and_pending_relationships_never_become_friends() {
        let values = json!([
            {"id":"1","type":1,"user":{"id":"1","username":"friend"}},
            {"id":"2","type":2,"user":{"id":"2","username":"blocked"}},
            {"id":"3","type":3,"user":{"id":"3","username":"pending"}}
        ]);
        let friends = parse_friends(&values).unwrap();
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].id, 1);
        assert!(
            parse_friends(&json!([{"id":"9","type":1,"user":{"id":"1"}}]))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn partial_empty_and_unsupported_channels_do_not_erase_valid_conversations() {
        let values = json!([
            {"id":"10","type":1,"recipients":[{"id":"20","username":"Friend"}]},
            {"id":"11","type":3,"recipients":[]},
            {"id":"12","type":1},
            {"id":"13","type":1,"recipients":["20"]},
            {"id":"14","type":1,"recipient_ids":["20"]},
            {"id":"15","type":18,"recipients":[{"id":"20"}]},
            {"id":"16","type":3,"recipients":[{"id":"20"},{"id":"30"}]},
            {"id":"17","type":1,"recipients":[{"id":"20"},{"id":"30"}]}
        ]);
        let parsed = parse_direct_channels(&values).unwrap();
        assert_eq!(
            parsed.channels.iter().map(|c| c.id).collect::<Vec<_>>(),
            [10, 16]
        );
        assert_eq!(parsed.skipped, 6);
        assert_eq!(parsed.channels[0].recipients[0].id, 20);
        assert!(parse_direct_channels(&json!({})).is_err());
        assert!(parse_direct_channels(&json!(vec![json!({}); MAX_SOCIAL + 1])).is_err());
    }
    #[test]
    fn malformed_accepted_relationship_does_not_erase_other_friends() {
        let parsed = parse_friends(&json!([
            {"type":1,"user":{"id":"0"}},
            {"type":1,"id":"90","user":{"id":"20"}},
            {"type":1,"id":"30","user":{"id":"30","username":"Friend"}}
        ]))
        .unwrap();
        assert_eq!(parsed.iter().map(|f| f.id).collect::<Vec<_>>(), [30]);
    }
    #[test]
    fn dm_response_cannot_redirect_to_guild_or_different_recipient() {
        let dm = json!({"id":"10","type":1,"recipients":[{"id":"20","username":"friend"}]});
        assert_eq!(parse_direct_channel(&dm).unwrap().recipients[0].id, 20);
        let mut invalid = dm.clone();
        invalid["guild_id"] = json!("50");
        assert!(parse_direct_channel(&invalid).is_err());
        invalid = dm;
        invalid["recipients"] = json!([]);
        assert!(parse_direct_channel(&invalid).is_err());
    }
}
