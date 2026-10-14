//! Delivery evidence is distinct from request success. Unknown never means safe to retry.
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Submitted,
    Queued,
    Delivered,
    Failed,
    Unknown,
}

impl State {
    pub fn from_message(message: &Value) -> Self {
        match message["status"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("delivered") => Self::Delivered,
            Some("queued" | "pending" | "sending") => Self::Queued,
            Some("failed") => Self::Failed,
            // An unrecognised Teams status is deliberately left unknown.
            Some(_) => Self::Unknown,
            None if message["source"] == "desktop" => Self::Submitted,
            None => Self::Unknown,
        }
    }
    pub fn notice(self) -> &'static str {
        match self {
            Self::Submitted => "Submitted to Teams; delivery is not confirmed.",
            Self::Queued => "Queued in Teams; delivery is not confirmed.",
            Self::Delivered => "Delivered.",
            Self::Failed => "Teams reports a failed send; check before resending.",
            Self::Unknown => "Delivery is unknown; check before resending.",
        }
    }
}

pub fn client_message_id() -> String {
    use rand::Rng;
    rand::rng()
        .random_range(100_000_000_000_000_000u64..1_000_000_000_000_000_000)
        .to_string()
}

pub fn valid_client_message_id(id: &str) -> bool {
    id.len() == 18 && id.bytes().all(|b| b.is_ascii_digit())
}

/// Reports only exact ID evidence from a bounded newest cache page. Absence is unknown.
pub fn reconcile(chat: &str, id: &str, messages: &[Value]) -> Value {
    let found = messages.iter().find(|m| m["clientMessageId"] == id);
    json!({"chatId":chat,"clientMessageId":id,"found":found.is_some(),
        "id":found.and_then(|m| m["id"].as_str()),
        "status":found.and_then(|m|m["status"].as_str()),
        "delivery_state":found.map(State::from_message).unwrap_or(State::Unknown),
        "source":"desktop_cache","scope":"newest_50_cached_messages","retryable":false})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn success_is_not_delivery_and_unrecognised_labels_remain_unknown() {
        for (message, expected) in [
            (json!({"source":"desktop"}), State::Submitted),
            (json!({"status":"Delivered"}), State::Delivered),
            (json!({"status":"Queued"}), State::Queued),
            (json!({"status":"SomeFutureStatus"}), State::Unknown),
            (json!({"status":"Failed"}), State::Failed),
        ] {
            assert_eq!(State::from_message(&message), expected);
        }
    }
    #[test]
    fn readback_matches_only_the_original_client_id_and_never_infers_failure() {
        let messages = vec![
            json!({"id":"1","clientMessageId":"100000000000000001","status":"Delivered","body":{"content":"private"}}),
        ];
        let found = reconcile("chat", "100000000000000001", &messages);
        assert_eq!(found["delivery_state"], "delivered");
        assert!(found.get("body").is_none());
        let missing = reconcile("chat", "100000000000000002", &messages);
        assert_eq!(missing["found"], false);
        assert_eq!(missing["delivery_state"], "unknown");
        assert_eq!(missing["retryable"], false);
    }
}
