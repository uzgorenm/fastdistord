//! Bounded, plain-text messaging data for the opt-in personal-account adapter.
//! This module performs no I/O. Sending requires an explicit account method call.
use crate::account::{display_name, snowflake};
use anyhow::{Result, bail};
use serde_json::{Value, json};

pub const MAX_MESSAGE_CHARS: usize = 2000;
pub const MAX_HISTORY_PAGE: usize = 50;
pub const MAX_HISTORY: usize = 200;
const MAX_REACTIONS: usize = 32;
const MAX_RECEIVED_CHARS: usize = 4000;
const MAX_CHANNELS: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChannel {
    pub id: u64,
    pub guild_id: u64,
    pub name: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: u64,
    pub channel_id: u64,
    pub author_id: u64,
    pub author_name: String,
    pub content: String,
    pub message_type: u16,
    pub call: Option<CallMetadata>,
    pub reference: Option<MessageReference>,
    pub edited: bool,
    pub reactions: Vec<ReactionSummary>,
    /// False when REST/event overlap prevents a trustworthy count until refresh.
    pub reactions_complete: bool,
    pub webhook: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageReference {
    pub message_id: u64,
    pub channel_id: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionSummary {
    /// Unicode emoji, or the API's `name:id` custom-emoji representation.
    pub emoji: String,
    pub count: u32,
    pub me: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageCapabilities {
    pub reply: bool,
    pub react: bool,
    pub edit: bool,
}
impl ChatMessage {
    /// Client-supported actions, not a cached assertion of channel permissions.
    /// Discord authoritatively checks READ_MESSAGE_HISTORY/SEND_MESSAGES/ADD_REACTIONS.
    pub fn capabilities(&self, own_id: Option<u64>) -> MessageCapabilities {
        let ordinary = matches!(self.message_type, 0 | 19) && self.id != 0 && self.channel_id != 0;
        MessageCapabilities {
            reply: ordinary,
            react: ordinary,
            edit: ordinary && !self.webhook && own_id == Some(self.author_id),
        }
    }
}
#[derive(Clone, Debug)]
pub struct HistoryPage {
    pub messages: Vec<ChatMessage>,
    /// A full page may have older entries; a short page ends this accessible history.
    pub has_more: bool,
}

const MAX_JOURNAL_EVENTS: usize = 256;
const MAX_JOURNAL_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug)]
enum HistoryMutation {
    Created(ChatMessage),
    Updated(Value),
    Deleted(Vec<u64>),
    Reaction(u64),
}

/// A per-request, channel-scoped replay window. REST responses have no Gateway
/// sequence, so counter deltas cannot be safely replayed; affected counts become
/// explicitly unknown. Only normalized bounded message fields are retained.
#[derive(Clone, Debug)]
pub struct HistoryJournal {
    channel: u64,
    mutations: Vec<HistoryMutation>,
    bytes: usize,
    valid: bool,
}
impl HistoryJournal {
    pub fn new(channel: u64) -> Self {
        Self {
            channel,
            mutations: Vec::new(),
            bytes: 0,
            valid: channel != 0,
        }
    }

    pub fn record(&mut self, kind: &str, data: &Value, _own: Option<u64>) {
        if !self.valid || snowflake(&data["channel_id"]) != Some(self.channel) {
            return;
        }
        let parsed: Result<Option<(HistoryMutation, usize)>> = (|| {
            Ok(Some(match kind {
                "MESSAGE_CREATE" => {
                    let message = parse_message(data, self.channel)?;
                    let bytes = message.content.len() + message.author_name.len() + 8 * 1024;
                    (HistoryMutation::Created(message), bytes)
                }
                "MESSAGE_UPDATE" => {
                    let value = normalized_update(data, self.channel)?;
                    let bytes = value.to_string().len();
                    (HistoryMutation::Updated(value), bytes)
                }
                "MESSAGE_DELETE" => {
                    let id = snowflake(&data["id"])
                        .ok_or_else(|| anyhow::anyhow!("Invalid deletion."))?;
                    (HistoryMutation::Deleted(vec![id]), 8)
                }
                "MESSAGE_DELETE_BULK" => {
                    let ids = data["ids"]
                        .as_array()
                        .filter(|v| v.len() <= 100)
                        .ok_or_else(|| anyhow::anyhow!("Invalid deletion batch."))?;
                    let ids: Vec<_> = ids
                        .iter()
                        .map(|v| snowflake(v).ok_or_else(|| anyhow::anyhow!("Invalid deletion.")))
                        .collect::<Result<_>>()?;
                    let bytes = ids.len() * 8;
                    (HistoryMutation::Deleted(ids), bytes)
                }
                "MESSAGE_REACTION_ADD"
                | "MESSAGE_REACTION_REMOVE"
                | "MESSAGE_REACTION_REMOVE_ALL"
                | "MESSAGE_REACTION_REMOVE_EMOJI" => {
                    let id = snowflake(&data["message_id"])
                        .ok_or_else(|| anyhow::anyhow!("Invalid reaction update."))?;
                    (HistoryMutation::Reaction(id), 8)
                }
                _ => return Ok(None),
            }))
        })();
        match parsed {
            Ok(Some((mutation, bytes)))
                if self.mutations.len() < MAX_JOURNAL_EVENTS
                    && self.bytes.saturating_add(bytes) <= MAX_JOURNAL_BYTES =>
            {
                self.bytes += bytes;
                self.mutations.push(mutation);
            }
            Ok(None) => {}
            _ => {
                self.valid = false;
                self.mutations.clear();
                self.bytes = 0;
            }
        }
    }

    pub fn reconcile(&self, mut messages: Vec<ChatMessage>) -> Result<Vec<ChatMessage>> {
        if !self.valid
            || messages.len() > MAX_HISTORY
            || messages.iter().any(|m| m.channel_id != self.channel)
        {
            bail!("Messages changed too quickly to reconcile safely. Refresh history.");
        }
        for mutation in &self.mutations {
            match mutation {
                HistoryMutation::Created(message) => {
                    if !messages.iter().any(|m| m.id == message.id) {
                        insert_message(&mut messages, message.clone());
                    }
                }
                HistoryMutation::Updated(value) => {
                    update_message(&mut messages, value, self.channel)?
                }
                HistoryMutation::Deleted(ids) => messages.retain(|m| !ids.contains(&m.id)),
                HistoryMutation::Reaction(id) => {
                    if let Some(message) = messages.iter_mut().find(|m| m.id == *id) {
                        message.reactions.clear();
                        message.reactions_complete = false;
                    }
                }
            }
        }
        messages.sort_by_key(|m| m.id);
        Ok(messages)
    }
}

fn normalized_update(data: &Value, channel: u64) -> Result<Value> {
    let id = snowflake(&data["id"]).ok_or_else(|| anyhow::anyhow!("Invalid message update."))?;
    let mut result = json!({"id":id, "channel_id":channel});
    if let Some(content) = data.get("content") {
        let content = content
            .as_str()
            .filter(|v| v.chars().count() <= MAX_RECEIVED_CHARS)
            .ok_or_else(|| anyhow::anyhow!("Invalid message update."))?;
        result["content"] = json!(content);
    }
    if let Some(kind) = data.get("type") {
        result["type"] = json!(
            kind.as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .ok_or_else(|| anyhow::anyhow!("Invalid message type."))?
        );
    }
    if let Some(author) = data.get("author") {
        let id =
            snowflake(&author["id"]).ok_or_else(|| anyhow::anyhow!("Invalid message author."))?;
        result["author"] = json!({"id":id,"global_name":display_name(author)});
    }
    if let Some(value) = data.get("edited_timestamp") {
        if !value.is_null() && !value.as_str().is_some_and(|s| s.len() <= 64) {
            bail!("Invalid message timestamp.");
        }
        result["edited_timestamp"] = value.clone();
    }
    if data.get("message_reference").is_some() {
        result["message_reference"] = match parse_reference(data.get("message_reference")) {
            Some(reference) => {
                json!({"message_id":reference.message_id,"channel_id":reference.channel_id})
            }
            None => Value::Null,
        };
    }
    if let Some(call) = data.get("call") {
        if call.is_null() {
            result["call"] = Value::Null;
        } else if let Some(parsed) = parse_call(Some(call)) {
            let mut normalized = json!({});
            if call.get("participants").is_some() {
                normalized["participants"] = json!(parsed.participants);
            }
            if let Some(timestamp) = call.get("ended_timestamp") {
                normalized["ended_timestamp"] = if timestamp.as_str().is_some_and(|s| s.len() <= 64)
                {
                    timestamp.clone()
                } else {
                    Value::Null
                };
            }
            result["call"] = normalized;
        }
    }
    if data.get("reactions").is_some() {
        let reactions = parse_reactions(data.get("reactions"))?;
        result["reactions"] = json!(
            reactions
                .iter()
                .map(|r| {
                    let emoji = if let Some((name, id)) = r.emoji.rsplit_once(':') {
                        json!({"id":id,"name":name})
                    } else {
                        json!({"id":null,"name":r.emoji})
                    };
                    json!({"emoji":emoji,"count":r.count,"me":r.me})
                })
                .collect::<Vec<_>>()
        );
    }
    Ok(result)
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

pub(crate) fn reply_payload(channel: u64, message: u64, content: &str) -> Result<Value> {
    validate_id(channel)?;
    validate_id(message)?;
    let mut payload = send_payload(content)?;
    payload["message_reference"] = json!({
        "type": 0, "channel_id": channel.to_string(),
        "message_id": message.to_string(), "fail_if_not_exists": true
    });
    Ok(payload)
}

pub(crate) fn edit_payload(message: &ChatMessage, own_id: u64, content: &str) -> Result<Value> {
    if !message.capabilities(Some(own_id)).edit {
        bail!("Only your own ordinary messages can be edited.");
    }
    let payload = send_payload(content)?;
    // Do not overwrite attachment, embed, component or flag state while editing text.
    Ok(json!({"content": payload["content"], "allowed_mentions": payload["allowed_mentions"]}))
}

/// Encode one URL path segment, including custom-emoji ':' and every UTF-8 byte.
pub(crate) fn reaction_segment(emoji: &str) -> Result<String> {
    if emoji.is_empty()
        || emoji.len() > 128
        || emoji.chars().any(char::is_control)
        || emoji.chars().any(char::is_whitespace)
    {
        bail!("Choose a valid emoji (at most 128 UTF-8 bytes).");
    }
    if let Some((name, id)) = emoji.split_once(':') {
        if name.is_empty()
            || name.len() > 32
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || snowflake(&Value::String(id.into())).is_none()
        {
            bail!("Custom emoji must use name:id.");
        }
    } else if emoji.is_ascii() {
        bail!("Choose a Unicode emoji or custom name:id.");
    }
    let mut encoded = String::with_capacity(emoji.len() * 3);
    const HEX: &[u8] = b"0123456789ABCDEF";
    for byte in emoji.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    Ok(encoded)
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
        channel_id,
        author_id,
        author_name: display_name(&value["author"]),
        content: content.to_owned(),
        message_type: value["type"]
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(0),
        call: parse_call(value.get("call")),
        reference: parse_reference(value.get("message_reference")),
        edited: value["edited_timestamp"].is_string(),
        reactions: parse_reactions(value.get("reactions"))?,
        reactions_complete: true,
        webhook: value.get("webhook_id").is_some_and(|v| !v.is_null()),
    })
}

fn parse_reference(value: Option<&Value>) -> Option<MessageReference> {
    let value = value?;
    // Forwards and system references are not represented as replies.
    if value["type"].as_u64().is_some_and(|kind| kind != 0) {
        return None;
    }
    Some(MessageReference {
        message_id: snowflake(&value["message_id"])?,
        channel_id: snowflake(&value["channel_id"])?,
    })
}

fn emoji_name(value: &Value) -> Option<String> {
    let name = value["name"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128);
    Some(if let Some(id) = snowflake(&value["id"]) {
        // Discord explicitly permits null names for deleted/unavailable custom
        // reaction emoji. Preserve the stable identity without dropping history.
        format!("{}:{id}", name.unwrap_or("unknown"))
    } else {
        name?.into()
    })
}

fn same_emoji(first: &str, second: &str) -> bool {
    match (first.rsplit_once(':'), second.rsplit_once(':')) {
        (Some((_, a)), Some((_, b))) => a == b,
        _ => first == second,
    }
}

fn parse_reactions(value: Option<&Value>) -> Result<Vec<ReactionSummary>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .filter(|a| a.len() <= MAX_REACTIONS)
        .ok_or_else(|| anyhow::anyhow!("Invalid or oversized message reactions."))?;
    values
        .iter()
        .map(|r| {
            // Normal reactions only; burst reactions are not added or conflated with them.
            let count = r.get("count_details").map_or(&r["count"], |d| &d["normal"]);
            Ok(ReactionSummary {
                emoji: emoji_name(&r["emoji"])
                    .ok_or_else(|| anyhow::anyhow!("Invalid reaction emoji."))?,
                count: count
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| anyhow::anyhow!("Invalid reaction count."))?,
                me: r["me"].as_bool().unwrap_or(false),
            })
        })
        .collect()
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
    if value.get("edited_timestamp").is_some() {
        updated.edited = value["edited_timestamp"].is_string();
    }
    if value.get("message_reference").is_some() {
        updated.reference = parse_reference(value.get("message_reference"));
    }
    if value.get("reactions").is_some() {
        updated.reactions = parse_reactions(value.get("reactions"))?;
        updated.reactions_complete = true;
    }
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
        messages.drain(..messages.len() - MAX_HISTORY);
    }
}
pub(crate) fn parse_messages(value: &Value, channel_id: u64) -> Result<Vec<ChatMessage>> {
    validate_id(channel_id)?;
    let Some(messages) = value.as_array() else {
        bail!("Discord returned invalid message history.");
    };
    if messages.len() > MAX_HISTORY_PAGE {
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

pub(crate) fn parse_page(value: &Value, channel: u64, before: Option<u64>) -> Result<HistoryPage> {
    if let Some(id) = before {
        validate_id(id)?;
    }
    let messages = parse_messages(value, channel)?;
    if before.is_some_and(|id| messages.iter().any(|m| m.id >= id)) {
        bail!("Discord returned history outside the requested page.");
    }
    Ok(HistoryPage {
        has_more: value
            .as_array()
            .is_some_and(|v| v.len() == MAX_HISTORY_PAGE),
        messages,
    })
}

/// Keep latest live state for overlaps. Stop loading older pages at the memory cap.
pub(crate) fn merge_history(messages: &mut Vec<ChatMessage>, older: Vec<ChatMessage>) {
    let channel = messages.first().or(older.first()).map(|m| m.channel_id);
    for message in older.into_iter().take(MAX_HISTORY_PAGE) {
        if Some(message.channel_id) == channel && !messages.iter().any(|m| m.id == message.id) {
            messages.push(message);
        }
    }
    messages.sort_by_key(|m| m.id);
    if messages.len() > MAX_HISTORY {
        messages.drain(..messages.len() - MAX_HISTORY);
    }
}

pub(crate) fn update_reactions(
    messages: &mut [ChatMessage],
    kind: &str,
    value: &Value,
    own: Option<u64>,
) {
    let (Some(channel), Some(id)) = (
        snowflake(&value["channel_id"]),
        snowflake(&value["message_id"]),
    ) else {
        return;
    };
    let Some(message) = messages
        .iter_mut()
        .find(|m| m.id == id && m.channel_id == channel)
    else {
        return;
    };
    if kind == "MESSAGE_REACTION_REMOVE_ALL" {
        message.reactions.clear();
        message.reactions_complete = true;
        return;
    }
    if !message.reactions_complete {
        return;
    }
    if value["burst"].as_bool() == Some(true) || value["type"].as_u64().is_some_and(|v| v != 0) {
        return;
    }
    let Some(emoji) = emoji_name(&value["emoji"]) else {
        return;
    };
    if kind == "MESSAGE_REACTION_REMOVE_EMOJI" {
        message.reactions.retain(|r| !same_emoji(&r.emoji, &emoji));
        return;
    }
    let is_me = own.is_some() && own == snowflake(&value["user_id"]);
    if let Some(reaction) = message
        .reactions
        .iter_mut()
        .find(|r| same_emoji(&r.emoji, &emoji))
    {
        match kind {
            "MESSAGE_REACTION_ADD" => {
                // Our REST result is never inserted optimistically, so the gateway increments once.
                reaction.count = reaction.count.saturating_add(1);
                if is_me {
                    reaction.me = true;
                }
            }
            "MESSAGE_REACTION_REMOVE" => {
                reaction.count = reaction.count.saturating_sub(1);
                if is_me {
                    reaction.me = false;
                }
            }
            _ => return,
        }
        message.reactions.retain(|r| r.count > 0);
    } else if kind == "MESSAGE_REACTION_ADD" && message.reactions.len() < MAX_REACTIONS {
        message.reactions.push(ReactionSummary {
            emoji,
            count: 1,
            me: is_me,
        });
    }
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
    // These protect actual message mutation/privacy requirements that the existing
    // send-only tests do not exercise; no network or automatic communications.
    #[test]
    fn reply_and_edit_preserve_mention_suppression_and_enforce_ownership() {
        let reply = reply_payload(10, 12, "@everyone <@20> hello").unwrap();
        assert_eq!(reply["message_reference"]["message_id"], "12");
        assert_eq!(reply["message_reference"]["channel_id"], "10");
        assert_eq!(reply["message_reference"]["fail_if_not_exists"], true);
        assert_eq!(reply["allowed_mentions"]["parse"], json!([]));
        assert_eq!(reply["allowed_mentions"]["replied_user"], false);
        let mut original = parse_message(&message(12), 10).unwrap();
        assert!(edit_payload(&original, 21, "other person's message").is_err());
        let edit = edit_payload(&original, 20, "edited @everyone").unwrap();
        assert_eq!(edit["allowed_mentions"]["parse"], json!([]));
        assert!(edit.get("flags").is_none() && edit.get("attachments").is_none());
        original.webhook = true;
        assert!(edit_payload(&original, 20, "no webhook edits").is_err());
        original.webhook = false;
        original.message_type = 3;
        assert_eq!(
            original.capabilities(Some(20)),
            MessageCapabilities::default()
        );
        assert!(reply_payload(0, 12, "bad channel").is_err());
    }
    #[test]
    fn reaction_paths_encode_utf8_and_never_escape_one_segment() {
        assert_eq!(reaction_segment("👍").unwrap(), "%F0%9F%91%8D");
        assert_eq!(reaction_segment("party:123").unwrap(), "party%3A123");
        assert_eq!(
            reaction_segment("👍/?#%").unwrap(),
            "%F0%9F%91%8D%2F%3F%23%25"
        );
        for bad in ["", "../@me", "x:0", "a:123/456", "👍\n", "has spaces"] {
            assert!(reaction_segment(bad).is_err());
        }
        assert!(reaction_segment(&"👍".repeat(33)).is_err());
    }
    #[test]
    fn partial_updates_preserve_reply_metadata_and_reactions_are_channel_scoped() {
        let mut value = message(12);
        value["message_reference"] = json!({"channel_id":"10", "message_id":"11"});
        value["reactions"] = json!([{"emoji":{"id":null,"name":"👍"},"count":3,"count_details":{"normal":2,"burst":1},"me":false}]);
        let mut messages = vec![parse_message(&value, 10).unwrap()];
        update_message(&mut messages, &json!({"channel_id":"10","id":"12","content":"edited","edited_timestamp":"2026-10-09T10:00:00Z"}), 10).unwrap();
        assert!(messages[0].edited);
        assert_eq!(messages[0].reference.as_ref().unwrap().message_id, 11);
        assert_eq!(messages[0].reactions[0].count, 2);
        let mut event =
            json!({"channel_id":"11","message_id":"12","user_id":"20","emoji":{"name":"👍"}});
        update_reactions(&mut messages, "MESSAGE_REACTION_ADD", &event, Some(20));
        assert_eq!(messages[0].reactions[0].count, 2);
        event["channel_id"] = json!("10");
        update_reactions(&mut messages, "MESSAGE_REACTION_ADD", &event, Some(20));
        assert_eq!(messages[0].reactions[0].count, 3);
        assert!(messages[0].reactions[0].me);
        update_reactions(&mut messages, "MESSAGE_REACTION_REMOVE", &event, Some(20));
        assert_eq!(messages[0].reactions[0].count, 2);
        assert!(!messages[0].reactions[0].me);
        let before = messages.clone();
        let bad = json!({"channel_id":"10","id":"12","content":"must not replace","reactions":[{"emoji":{},"count":2}]});
        assert!(update_message(&mut messages, &bad, 10).is_err());
        assert_eq!(messages, before);
        // Emoji names can change or disappear while their stable custom IDs remain.
        messages[0].reactions.push(ReactionSummary {
            emoji: "old_name:123".into(),
            count: 2,
            me: false,
        });
        let renamed = json!({"channel_id":"10","message_id":"12","user_id":"20","emoji":{"name":"new_name","id":"123"}});
        update_reactions(&mut messages, "MESSAGE_REACTION_ADD", &renamed, Some(20));
        assert_eq!(messages[0].reactions.len(), 2);
        assert_eq!(messages[0].reactions[1].count, 3);
        let deleted = json!({"channel_id":"10","message_id":"12","emoji":{"name":null,"id":"123"}});
        update_reactions(
            &mut messages,
            "MESSAGE_REACTION_REMOVE_EMOJI",
            &deleted,
            Some(20),
        );
        assert_eq!(messages[0].reactions.len(), 1);
    }
    #[test]
    fn older_pages_obey_cursor_and_keep_newer_edits_at_bounded_memory() {
        let mut messages: Vec<_> = (151..=200)
            .map(|id| parse_message(&message(id), 10).unwrap())
            .collect();
        messages[0].content = "newer live edit".into();
        let page = parse_page(
            &json!((101..=150).map(message).collect::<Vec<_>>()),
            10,
            Some(151),
        )
        .unwrap();
        assert!(page.has_more);
        merge_history(&mut messages, page.messages);
        let overlap = parse_message(&message(151), 10).unwrap();
        merge_history(&mut messages, vec![overlap]);
        assert_eq!(
            messages.iter().find(|m| m.id == 151).unwrap().content,
            "newer live edit"
        );
        merge_history(
            &mut messages,
            (51..=100)
                .map(|id| parse_message(&message(id), 10).unwrap())
                .collect(),
        );
        merge_history(
            &mut messages,
            (1..=50)
                .map(|id| parse_message(&message(id), 10).unwrap())
                .collect(),
        );
        assert_eq!(messages.len(), MAX_HISTORY);
        insert_message(&mut messages, parse_message(&message(201), 10).unwrap());
        assert_eq!(messages.len(), MAX_HISTORY);
        assert_eq!(messages[0].id, 2);
        assert!(parse_page(&json!([message(151)]), 10, Some(151)).is_err());
        assert!(parse_page(&json!([]), 10, Some(0)).is_err());
        assert!(!parse_page(&json!([]), 10, Some(1)).unwrap().has_more);
    }
    #[test]
    fn in_flight_history_preserves_new_edits_deletes_and_creates_without_doubling_reactions() {
        let mut journal = HistoryJournal::new(10);
        journal.record("MESSAGE_UPDATE", &json!({"channel_id":"10","id":"10","content":"new edit","edited_timestamp":"2026-10-09T12:00:00Z"}), Some(20));
        journal.record(
            "MESSAGE_DELETE",
            &json!({"channel_id":"10","id":"11"}),
            Some(20),
        );
        journal.record("MESSAGE_CREATE", &message(12), Some(20));
        journal.record(
            "MESSAGE_DELETE",
            &json!({"channel_id":"99","id":"10"}),
            Some(20),
        );
        journal.record(
            "MESSAGE_REACTION_ADD",
            &json!({"channel_id":"10","message_id":"10","emoji":{"name":"👍"},"user_id":"20"}),
            Some(20),
        );
        let mut old = message(10);
        old["reactions"] = json!([{"emoji":{"name":"👍"},"count":1,"me":true}]);
        let page = parse_messages(&json!([old, message(11)]), 10).unwrap();
        let reconciled = journal.reconcile(page).unwrap();
        assert_eq!(
            reconciled.iter().map(|m| m.id).collect::<Vec<_>>(),
            [10, 12]
        );
        assert_eq!(reconciled[0].content, "new edit");
        assert!(reconciled[0].edited);
        assert!(!reconciled[0].reactions_complete);
        assert!(reconciled[0].reactions.is_empty());
        // The same deletion also prevents a late edit response from resurrecting it.
        let edited = parse_message(&message(11), 10).unwrap();
        assert!(
            !journal
                .reconcile(vec![edited])
                .unwrap()
                .iter()
                .any(|m| m.id == 11)
        );
    }
    #[test]
    fn reconciliation_overflow_invalid_scope_or_malformed_changes_never_replace_history() {
        let mut journal = HistoryJournal::new(10);
        for _ in 0..=MAX_JOURNAL_EVENTS {
            journal.record("MESSAGE_DELETE", &json!({"channel_id":"10","id":"9"}), None);
        }
        assert!(
            journal
                .reconcile(vec![parse_message(&message(10), 10).unwrap()])
                .is_err()
        );
        let mut malformed = HistoryJournal::new(10);
        malformed.record(
            "MESSAGE_UPDATE",
            &json!({"channel_id":"10","id":"9","content":"x".repeat(MAX_RECEIVED_CHARS+1)}),
            None,
        );
        assert!(malformed.reconcile(Vec::new()).is_err());
        let wrong_channel = HistoryJournal::new(11);
        assert!(
            wrong_channel
                .reconcile(vec![parse_message(&message(10), 10).unwrap()])
                .is_err()
        );
        let mut bytes = HistoryJournal::new(10);
        let mut large = message(1);
        large["content"] = json!("🦀".repeat(MAX_RECEIVED_CHARS));
        for _ in 0..MAX_JOURNAL_EVENTS {
            bytes.record("MESSAGE_CREATE", &large, None);
        }
        assert!(bytes.reconcile(Vec::new()).is_err());
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
