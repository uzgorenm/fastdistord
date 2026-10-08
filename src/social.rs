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
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectChannel {
    pub id: u64,
    pub name: String,
    pub recipients: Vec<Friend>,
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
            bail!("Discord returned an invalid friend.");
        };
        if relationship
            .get("id")
            .is_some_and(|v| snowflake(v) != Some(id))
        {
            bail!("Discord returned inconsistent friend IDs.");
        }
        if seen.insert(id) {
            friends.push(Friend {
                id,
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
pub fn parse_direct_channels(value: &Value) -> Result<Vec<DirectChannel>> {
    let Some(values) = value.as_array().filter(|v| v.len() <= MAX_SOCIAL) else {
        bail!("Discord returned invalid or oversized conversations.");
    };
    let mut seen = HashSet::new();
    let mut channels = Vec::new();
    for value in values {
        if !matches!(value["type"].as_u64(), Some(1 | 3)) {
            continue;
        }
        let channel = parse_direct_channel(value)?;
        if seen.insert(channel.id) {
            channels.push(channel);
        }
    }
    Ok(channels)
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
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
        assert!(parse_friends(&json!([{"id":"9","type":1,"user":{"id":"1"}}])).is_err());
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
