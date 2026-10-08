//! Bounded, plain-text messaging data for the opt-in personal-account adapter.
//! This module performs no I/O. Sending requires an explicit account method call.
use crate::account::{display_name, snowflake};
use anyhow::{Result, bail};
use serde_json::{Value, json};

pub const MAX_MESSAGE_CHARS: usize = 2000;
pub const MAX_HISTORY: usize = 50;
const MAX_RECEIVED_CHARS: usize = 4000;
const MAX_CHANNELS: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChannel {
    pub id: u64,
    pub guild_id: u64,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: u64,
    pub author_id: u64,
    pub author_name: String,
    pub content: String,
}

pub(crate) fn validate_id(id: u64) -> Result<()> {
    if id == 0 {
        bail!("Choose a valid server or channel.");
    }
    Ok(())
}

pub(crate) fn send_payload(content: &str) -> Result<Value> {
    if content.trim().is_empty() {
        bail!("Enter a message before sending.");
    }
    if content.chars().count() > MAX_MESSAGE_CHARS {
        bail!("Messages must contain at most 2000 characters.");
    }
    if content.contains('\0') {
        bail!("Messages cannot contain a null character.");
    }
    // Suppress notifications for @everyone, roles and users, including literal
    // mention syntax. Also suppress link embeds; this client shows plain text.
    Ok(json!({
        "content": content,
        "tts": false,
        "allowed_mentions": {"parse": [], "replied_user": false},
        "flags": 4
    }))
}

pub(crate) fn parse_channels(value: &Value, guild_id: u64) -> Result<Vec<TextChannel>> {
    validate_id(guild_id)?;
    let Some(channels) = value.as_array() else {
        bail!("Discord returned invalid text channels.");
    };
    if channels.len() > MAX_CHANNELS {
        bail!("Discord text channels exceeded the safety limit.");
    }
    let mut result = Vec::new();
    for channel in channels {
        // Guild text and announcement channels only; forums/threads need separate UX.
        if !matches!(channel["type"].as_u64(), Some(0 | 5)) {
            continue;
        }
        let (Some(id), Some(name)) = (snowflake(&channel["id"]), channel["name"].as_str()) else {
            bail!("Discord returned an invalid text channel.");
        };
        if name.is_empty() || name.chars().count() > 100 {
            bail!("Discord returned an invalid text channel name.");
        }
        if channel
            .get("guild_id")
            .is_some_and(|id| snowflake(id) != Some(guild_id))
        {
            bail!("Discord returned a text channel from a different server.");
        }
        result.push(TextChannel {
            id,
            guild_id,
            name: name.to_owned(),
        });
    }
    Ok(result)
}

pub(crate) fn parse_message(value: &Value, channel_id: u64) -> Result<ChatMessage> {
    validate_id(channel_id)?;
    let (Some(id), Some(author_id), Some(content)) = (
        snowflake(&value["id"]),
        snowflake(&value["author"]["id"]),
        value["content"].as_str(),
    ) else {
        bail!("Discord returned an invalid message.");
    };
    if snowflake(&value["channel_id"]) != Some(channel_id) {
        bail!("Discord returned a message from a different channel.");
    }
    if content.chars().count() > MAX_RECEIVED_CHARS {
        bail!("Discord message exceeded the safety limit.");
    }
    Ok(ChatMessage {
        id,
        author_id,
        author_name: display_name(&value["author"]),
        content: content.to_owned(),
    })
}

pub(crate) fn parse_messages(value: &Value, channel_id: u64) -> Result<Vec<ChatMessage>> {
    validate_id(channel_id)?;
    let Some(messages) = value.as_array() else {
        bail!("Discord returned invalid message history.");
    };
    if messages.len() > MAX_HISTORY {
        bail!("Discord message history exceeded the safety limit.");
    }
    let mut result: Vec<_> = messages
        .iter()
        .map(|message| parse_message(message, channel_id))
        .collect::<Result<_>>()?;
    result.reverse(); // Discord returns newest first; the UI reads oldest first.
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(id: u64) -> Value {
        json!({"id":id.to_string(), "channel_id":"10", "author":{"id":"20", "username":"Tester"}, "content":"hello"})
    }
    #[test]
    fn send_preserves_unicode_and_never_enables_mentions_or_tts() {
        let content = format!("{} @everyone <@20>", "é".repeat(20));
        let payload = send_payload(&content).unwrap();
        assert_eq!(payload["content"], content);
        assert_eq!(payload["allowed_mentions"]["parse"], json!([]));
        assert_eq!(payload["allowed_mentions"]["replied_user"], false);
        assert_eq!(payload["tts"], false);
        assert_eq!(payload["flags"], 4);
        assert!(send_payload(&"🦀".repeat(2000)).is_ok());
        assert!(send_payload(&"🦀".repeat(2001)).is_err());
    }
    #[test]
    fn invalid_send_errors_do_not_echo_message_content() {
        for content in ["", " \n\t", "sensitive\0payload"] {
            let error = send_payload(content).unwrap_err().to_string();
            assert!(!error.contains("sensitive"));
        }
        assert!(validate_id(0).is_err());
    }
    #[test]
    fn history_is_bounded_ordered_and_channel_checked() {
        let history = parse_messages(&json!([message(3), message(2)]), 10).unwrap();
        assert_eq!(history.iter().map(|m| m.id).collect::<Vec<_>>(), [2, 3]);
        assert!(parse_messages(&json!([message(3)]), 11).is_err());
        assert!(parse_messages(&json!(vec![message(3); 51]), 10).is_err());
        assert!(parse_messages(&json!(null), 10).is_err());
        assert!(parse_messages(&json!([]), 0).is_err());
    }
    #[test]
    fn channel_filter_and_invalid_snowflakes() {
        let channels = json!([
            {"id":"1", "type":0, "name":"general", "guild_id":"9"},
            {"id":"2", "type":2, "name":"voice"},
            {"id":"3", "type":5, "name":"announcements"}
        ]);
        let result = parse_channels(&channels, 9).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[1].id, 3);
        assert!(parse_channels(&channels, 8).is_err());
        assert!(parse_channels(&json!([{"id":"0", "type":0, "name":"bad"}]), 9).is_err());
        assert!(parse_channels(&json!([{"id":"secret", "type":0, "name":"bad"}]), 9).is_err());
    }
    #[test]
    fn received_fields_are_bounded_and_malformed_data_is_not_exposed() {
        let mut value = message(1);
        value["content"] = json!("x".repeat(4001));
        assert!(parse_message(&value, 10).is_err());
        value["content"] = json!(""); // Attachments/system messages can have no text.
        value["author"]["global_name"] = json!("a".repeat(200));
        assert_eq!(parse_message(&value, 10).unwrap().author_name.len(), 100);
        value["id"] = json!("sensitive-invalid-id");
        assert!(
            !parse_message(&value, 10)
                .unwrap_err()
                .to_string()
                .contains("sensitive")
        );
    }
}
