//! Visual grouping only: messages retain their own identity and contents.
use crate::messaging::ChatMessage;

const GROUP_GAP_MS: u64 = 5 * 60 * 1000;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;
fn created_ms(message: &ChatMessage) -> u64 {
    (message.id >> 22) + DISCORD_EPOCH_MS
}
/// Recompute from adjacent visible records so history and incremental updates agree.
/// Replies/commands and system events keep their own header/context boundaries.
pub(super) fn continues(previous: Option<&ChatMessage>, current: &ChatMessage) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if previous.author_id == 0
        || previous.author_id != current.author_id
        || previous.message_type != 0
        || current.message_type != 0
        || previous.id >= current.id
    {
        return false;
    }
    let previous_ms = created_ms(previous);
    let current_ms = created_ms(current);
    previous_ms / DAY_MS == current_ms / DAY_MS
        && current_ms.saturating_sub(previous_ms) <= GROUP_GAP_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(ms: u64, sequence: u64, author: u64) -> ChatMessage {
        ChatMessage {
            id: ((ms - DISCORD_EPOCH_MS) << 22) | sequence,
            author_id: author,
            author_name: "Same display name".into(),
            content: "A distinct message".into(),
            message_type: 0,
            call: None,
            channel_id: 20,
            ..Default::default()
        }
    }
    const START: u64 = 1_800_000_000_000;
    #[test]
    fn same_identity_continues_without_merging_or_using_display_name() {
        let first = message(START, 1, 10);
        let mut second = message(START + 1000, 1, 10);
        second.author_name = "Changed display name".into();
        second.content.clear(); // Attachment-only messages stay distinct continuation rows.
        assert!(continues(Some(&first), &second));
        second.author_id = 20;
        second.author_name = first.author_name.clone();
        assert!(!continues(Some(&first), &second));
        assert!(!continues(None, &first));
        assert!(!continues(Some(&first), &first));
    }
    #[test]
    fn time_gap_and_utc_day_restart_headers() {
        let first = message(START, 1, 10);
        assert!(continues(
            Some(&first),
            &message(START + GROUP_GAP_MS, 1, 10)
        ));
        assert!(!continues(
            Some(&first),
            &message(START + GROUP_GAP_MS + 1, 1, 10)
        ));
        let midnight = (START / DAY_MS + 1) * DAY_MS;
        assert!(!continues(
            Some(&message(midnight - 1, 1, 10)),
            &message(midnight, 1, 10)
        ));
        assert!(!continues(Some(&first), &message(START - 1, 1, 10)));
    }
    #[test]
    fn system_reply_and_command_boundaries_restart_both_sides() {
        let first = message(START, 1, 10);
        let last = message(START + 2000, 1, 10);
        for kind in [1, 3, 7, 19, 20, 999] {
            let mut boundary = message(START + 1000, 1, 10);
            boundary.message_type = kind;
            assert!(!continues(Some(&first), &boundary));
            assert!(!continues(Some(&boundary), &last));
        }
    }
    #[test]
    fn live_insert_edit_delete_and_history_edges_recompute_consistently() {
        let first = message(START, 1, 10);
        let second = message(START + 1000, 1, 10);
        let third = message(START + 2000, 1, 10);
        let mut messages = vec![first.clone()];
        crate::messaging::insert_message(&mut messages, third.clone());
        crate::messaging::insert_message(&mut messages, second.clone());
        assert_eq!(messages, vec![first, second, third]);
        assert!(continues(Some(&messages[0]), &messages[1]));
        let update = serde_json::json!({
            "id": messages[1].id.to_string(), "channel_id":"20", "content":"Edited body"
        });
        crate::messaging::update_message(&mut messages, &update, 20).unwrap();
        assert!(continues(Some(&messages[0]), &messages[1]));
        messages.remove(1); // Deletion groups the actual surviving neighbors.
        assert!(continues(Some(&messages[0]), &messages[1]));
        messages.remove(0); // Trimmed history must still show its first visible author.
        assert!(!continues(None, &messages[0]));
    }
}
