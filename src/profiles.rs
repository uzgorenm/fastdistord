//! Public profile metadata only. Missing presence stays unknown.
use serde_json::Value;
use std::collections::HashMap;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Presence {
    #[default]
    Unknown,
    Online,
    Idle,
    Dnd,
    Offline,
}
impl Presence {
    pub fn parse(value: &Value) -> Self {
        match value.as_str() {
            Some("online") => Self::Online,
            Some("idle") => Self::Idle,
            Some("dnd") => Self::Dnd,
            Some("offline" | "invisible") => Self::Offline,
            _ => Self::Unknown,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Status unknown",
            Self::Online => "Online",
            Self::Idle => "Idle",
            Self::Dnd => "Do not disturb",
            Self::Offline => "Offline",
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub avatar: Option<String>,
    pub presence: Presence,
}
pub fn hash(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let plain = s.strip_prefix("a_").unwrap_or(s);
    (plain.len() == 32 && plain.bytes().all(|c| c.is_ascii_hexdigit())).then(|| s.to_owned())
}
pub fn observe(map: &mut HashMap<u64, Profile>, user: &Value) {
    let Some(id) = crate::account::snowflake(&user["id"]) else {
        return;
    };
    if map.len() >= 2048 && !map.contains_key(&id) {
        return;
    }
    let profile = map.entry(id).or_default();
    if let Some(v) = user.get("avatar") {
        profile.avatar = hash(v);
    }
}
pub fn presence(map: &mut HashMap<u64, Profile>, data: &Value) {
    observe(map, &data["user"]);
    if let Some(id) = crate::account::snowflake(&data["user"]["id"])
        && let Some(profile) = map.get_mut(&id)
    {
        profile.presence = Presence::parse(&data["status"]);
    }
}
/// Only the server's aggregate session, or its single session, establishes self status.
/// Session IDs are examined locally and never retained or exported.
pub fn self_presence(sessions: &Value) -> Presence {
    let Some(list) = sessions.as_array().filter(|a| a.len() <= 16) else {
        return Presence::Unknown;
    };
    let item = list
        .iter()
        .find(|s| s["session_id"].as_str() == Some("all"))
        .or_else(|| (list.len() == 1).then(|| &list[0]));
    item.map_or(Presence::Unknown, |s| Presence::parse(&s["status"]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn partial_presence_preserves_avatar_and_missing_status_is_unknown() {
        let mut map = HashMap::new();
        observe(
            &mut map,
            &json!({"id":"42", "avatar":"0123456789abcdef0123456789abcdef"}),
        );
        presence(&mut map, &json!({"user":{"id":"42"}, "status":"online"}));
        assert!(map[&42].avatar.is_some());
        assert_eq!(map[&42].presence, Presence::Online);
        presence(&mut map, &json!({"user":{"id":"42"}}));
        assert_eq!(map[&42].presence, Presence::Unknown);
        assert_eq!(
            self_presence(
                &json!([{ "session_id":"a", "status":"online" }, { "session_id":"b", "status":"idle" }])
            ),
            Presence::Unknown
        );
        assert_eq!(
            self_presence(
                &json!([{ "session_id":"a", "status":"online" }, { "session_id":"all", "status":"dnd" }])
            ),
            Presence::Dnd
        );
        assert_eq!(Presence::parse(&json!("invisible")), Presence::Offline);
    }
}
