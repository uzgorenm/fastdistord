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
    pub message_type: u16,
    pub call: Option<CallMetadata>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallMetadata {
    pub participants: Option<Vec<u64>>,
    pub ended: bool,
    pub ended_ms: Option<i128>,
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
        message_type: value["type"]
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(0),
        call: parse_call(value.get("call")),
    })
}

fn parse_call(value: Option<&Value>) -> Option<CallMetadata> {
    let call = value?.as_object()?;
    let participants = call
        .get("participants")
        .and_then(Value::as_array)
        .filter(|v| v.len() <= 250)
        .and_then(|v| v.iter().map(snowflake).collect::<Option<Vec<_>>>());

    let ended_ms = call
        .get("ended_timestamp")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 64)
        .and_then(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
        })
        .map(|t| t.unix_timestamp_nanos() / 1_000_000);
    Some(CallMetadata {
        participants,
        ended: ended_ms.is_some(),
        ended_ms,
    })
}
pub(crate) fn system_label(message: &ChatMessage, own: Option<u64>) -> Option<String> {
    let author = if own == Some(message.author_id) {
        "You"
    } else {
        &message.author_name
    };
    Some(match message.message_type {
        0 | 19 | 20 => return None,
        3 => {
            let call = message.call.as_ref();
            let ended = call.is_some_and(|c| c.ended);
            let missed = ended
                && own.is_some_and(|id| {
                    id != message.author_id
                        && call
                            .and_then(|c| c.participants.as_ref())
                            .is_some_and(|p| !p.contains(&id))
                });
            let duration = call
                .and_then(|c| c.ended_ms)
                .and_then(|end| end.checked_sub(i128::from((message.id >> 22) + 1_420_070_400_000)))
                .filter(|ms| *ms >= 0)
                .map(|ms| ms / 1000);
            let mut text = if missed {
                format!("You missed a call from {author}")
            } else {
                format!("{author} started a call")
            };
            if ended {
                if let Some(seconds) = duration {
                    text.push_str(&format!(
                        " that lasted {}:{:02}",
                        seconds / 60,
                        seconds % 60
                    ));
                } else {
                    text.push_str(" · ended");
                }
            }
            if let Some(participants) = call.and_then(|c| c.participants.as_ref()) {
                text.push_str(&format!(" · {} participants", participants.len()));
            }
            text
        }
        1 => format!("{author} added a recipient"),
        2 => format!("{author} removed a recipient"),
        4 => {
            if message.content.is_empty() {
                format!("{author} removed the channel name")
            } else {
                format!("{author} changed the channel name: {}", message.content)
            }
        }
        5 => format!("{author} changed the channel icon"),
        6 => format!("{author} pinned a message"),
        7 => format!("{author} joined the server"),
        n => format!("Discord system message (type {n})"),
    })
}
/// Channel-scoped partial updates replace the existing ID; missing metadata is preserved.
pub(crate) fn update_message(
    messages: &mut [ChatMessage],
    value: &Value,
    channel: u64,
) -> Result<()> {
    if snowflake(&value["channel_id"]) != Some(channel) {
        bail!("Message update belongs to another channel");
    }
    let id = snowflake(&value["id"]).ok_or_else(|| anyhow::anyhow!("Invalid message update"))?;
    let Some(existing) = messages.iter_mut().find(|m| m.id == id) else {
        return Ok(());
    };
    let mut updated = existing.clone();
    if let Some(content) = value.get("content") {
        let content = content
            .as_str()
            .filter(|v| v.chars().count() <= MAX_RECEIVED_CHARS)
            .ok_or_else(|| anyhow::anyhow!("Invalid message update content"))?;
        updated.content = content.to_owned();
    }
    if let Some(kind) = value.get("type") {
        updated.message_type = kind
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .ok_or_else(|| anyhow::anyhow!("Invalid message type"))?;
    }
    if let Some(author) = value.get("author")
        && let Some(id) = snowflake(&author["id"])
    {
        updated.author_id = id;
        updated.author_name = display_name(author);
    }
    if let Some(call) = value.get("call") {
        if call.is_null() {
            updated.call = None;
        } else if let Some(new) = parse_call(Some(call)) {
            let prior = updated.call.take();
            updated.call = Some(CallMetadata {
                participants: if call.get("participants").is_some() {
                    new.participants
                } else {
                    prior.as_ref().and_then(|c| c.participants.clone())
                },
                ended: if call.get("ended_timestamp").is_some() {
                    new.ended
                } else {
                    prior.as_ref().is_some_and(|c| c.ended)
                },
                ended_ms: if call.get("ended_timestamp").is_some() {
                    new.ended_ms
                } else {
                    prior.and_then(|c| c.ended_ms)
                },
            });
        }
    }
    *existing = updated;
    Ok(())
}
pub(crate) fn insert_message(messages: &mut Vec<ChatMessage>, message: ChatMessage) {
    messages.retain(|m| m.id != message.id);
    messages.push(message);
    messages.sort_by_key(|m| m.id);
    if messages.len() > MAX_HISTORY {
        messages.remove(0);
    }
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
    result.sort_by_key(|m| m.id);
    result.dedup_by_key(|m| m.id);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(id: u64) -> Value {
        json!({"id":id.to_string(), "channel_id":"10", "author":{"id":"20", "username":"Tester"}, "content":"hello"})
    }
    #[test]
    fn real_call_message_updates_in_place_and_history_create_deduplicates() {
        let id = 1_000_000_000_000_000_000u64;
        let value = json!({"id":id.to_string(),"channel_id":"10","author":{"id":"20","username":"Tester"},"type":3,"content":"","call":{"participants":["20","30"],"ended_timestamp":null}});
        let mut messages = parse_messages(&json!([value.clone(), value.clone()]), 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            system_label(&messages[0], Some(20)).unwrap(),
            "You started a call · 2 participants"
        );
        insert_message(&mut messages, parse_message(&value, 10).unwrap());
        assert_eq!(messages.len(), 1);
        let end = time::OffsetDateTime::from_unix_timestamp_nanos(
            i128::from((id >> 22) + 1_420_070_400_000 + 62_000) * 1_000_000,
        )
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
        update_message(
            &mut messages,
            &json!({"id":id.to_string(),"channel_id":"10","call":{"ended_timestamp":end}}),
            10,
        )
        .unwrap();
        assert_eq!(messages.len(), 1);
        assert!(
            system_label(&messages[0], Some(30))
                .unwrap()
                .contains("lasted 1:02")
        );
        assert!(
            system_label(&messages[0], Some(40))
                .unwrap()
                .starts_with("You missed a call from Tester")
        );
        assert_eq!(
            messages[0]
                .call
                .as_ref()
                .unwrap()
                .participants
                .as_ref()
                .unwrap()
                .len(),
            2
        );
        let before = messages.clone();
        assert!(
            update_message(
                &mut messages,
                &json!({"id":id.to_string(),"channel_id":"11","content":"wrong scope"}),
                10
            )
            .is_err()
        );
        assert_eq!(messages, before);
        update_message(
            &mut messages,
            &json!({"id":"5","channel_id":"10","content":"unknown"}),
            10,
        )
        .unwrap();
        assert_eq!(messages, before);
    }
    #[test]
    fn missing_or_invalid_call_metadata_never_invents_missed_or_duration() {
        let mut value = message(1);
        value["type"] = json!(3);
        value["content"] = json!("");
        let parsed = parse_message(&value, 10).unwrap();
        assert_eq!(
            system_label(&parsed, Some(30)).unwrap(),
            "Tester started a call"
        );
        value["call"] = json!({"ended_timestamp":"malformed","participants":["invalid"]});
        let label = system_label(&parse_message(&value, 10).unwrap(), Some(30)).unwrap();
        assert!(!label.contains("missed") && !label.contains("lasted") && !label.contains("ended"));
        value["type"] = json!(6);
        assert_eq!(
            system_label(&parse_message(&value, 10).unwrap(), None).unwrap(),
            "Tester pinned a message"
        );
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
