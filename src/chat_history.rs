use std::collections::VecDeque;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;

pub const MAX_HISTORY_LENGTH: u64 = 5_000;
pub const DEFAULT_HISTORY_LENGTH: u64 = 200;
pub const MAX_TOOL_RESULT_MESSAGES: usize = 200;
const DEFAULT_TOOL_RESULT_MESSAGES: usize = 50;

pub type ChatHistory = Arc<tokio::sync::Mutex<ChatHistoryBuffer>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatHistorySource {
    User,
    Bot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatHistoryEntry {
    pub seq: u64,
    pub username: String,
    pub text: String,
    pub source: ChatHistorySource,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug)]
pub struct ChatHistoryBuffer {
    capacity: usize,
    next_seq: u64,
    entries: VecDeque<ChatHistoryEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatHistoryQuery {
    pub limit: Option<usize>,
    pub user: Option<String>,
    pub contains: Option<String>,
    pub before_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatHistoryPage {
    pub messages: Vec<ChatHistoryEntry>,
    pub has_more: bool,
    pub next_before_seq: Option<u64>,
}

impl ChatHistoryBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            next_seq: 1,
            entries: VecDeque::with_capacity(capacity),
        }
    }

    pub fn from_prefill<I>(capacity: usize, messages: I) -> Self
    where
        I: IntoIterator<Item = (String, String, DateTime<Utc>)>,
    {
        let mut items: Vec<(String, String, DateTime<Utc>)> = messages.into_iter().collect();
        if items.len() > capacity {
            items.drain(..items.len() - capacity);
        }

        let mut buffer = Self::new(capacity);
        for (username, text, timestamp) in items {
            buffer.push_user_at(username, text, timestamp);
        }
        buffer
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn push_user(&mut self, username: impl Into<String>, text: impl Into<String>) {
        self.push_user_at(username, text, Utc::now());
    }

    pub fn push_user_at(
        &mut self,
        username: impl Into<String>,
        text: impl Into<String>,
        timestamp: DateTime<Utc>,
    ) {
        self.push(username, text, ChatHistorySource::User, timestamp);
    }

    pub fn push_bot(&mut self, username: impl Into<String>, text: impl Into<String>) {
        self.push_bot_at(username, text, Utc::now());
    }

    pub fn push_bot_at(
        &mut self,
        username: impl Into<String>,
        text: impl Into<String>,
        timestamp: DateTime<Utc>,
    ) {
        self.push(username, text, ChatHistorySource::Bot, timestamp);
    }

    fn push(
        &mut self,
        username: impl Into<String>,
        text: impl Into<String>,
        source: ChatHistorySource,
        timestamp: DateTime<Utc>,
    ) {
        if self.capacity == 0 {
            return;
        }
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.entries.push_back(ChatHistoryEntry {
            seq,
            username: username.into(),
            text: text.into(),
            source,
            timestamp,
        });
    }

    pub fn snapshot(&self) -> Vec<ChatHistoryEntry> {
        self.entries.iter().cloned().collect()
    }

    pub fn query(&self, query: ChatHistoryQuery) -> ChatHistoryPage {
        let limit = query
            .limit
            .unwrap_or(DEFAULT_TOOL_RESULT_MESSAGES)
            .clamp(1, MAX_TOOL_RESULT_MESSAGES);
        let user = query
            .user
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase);
        let contains = query
            .contains
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase);

        let mut newest_first = Vec::with_capacity(limit.saturating_add(1));
        for entry in self.entries.iter().rev() {
            if query.before_seq.is_some_and(|seq| entry.seq >= seq) {
                continue;
            }
            if let Some(ref user) = user
                && entry.username.to_ascii_lowercase() != *user
            {
                continue;
            }
            if let Some(ref needle) = contains
                && !entry.text.to_ascii_lowercase().contains(needle)
            {
                continue;
            }

            newest_first.push(entry.clone());
            if newest_first.len() > limit {
                break;
            }
        }

        let has_more = newest_first.len() > limit;
        if has_more {
            newest_first.truncate(limit);
        }
        newest_first.reverse();

        let next_before_seq = if has_more {
            newest_first.first().map(|entry| entry.seq)
        } else {
            None
        };
        ChatHistoryPage {
            messages: newest_first,
            has_more,
            next_before_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_buffer() -> ChatHistoryBuffer {
        let mut buffer = ChatHistoryBuffer::new(10);
        buffer.push_user("alice", "hello chat");
        buffer.push_user("bob", "weather is bad");
        buffer.push_user("alice", "weather got better");
        buffer.push_bot("bot", "I can check that");
        buffer.push_user("carol", "different topic");
        buffer
    }

    #[test]
    fn query_returns_recent_messages_chronologically() {
        let buffer = sample_buffer();
        let page = buffer.query(ChatHistoryQuery {
            limit: Some(3),
            ..ChatHistoryQuery::default()
        });

        assert_eq!(
            page.messages
                .iter()
                .map(|entry| entry.username.as_str())
                .collect::<Vec<_>>(),
            vec!["alice", "bot", "carol"]
        );
        assert!(page.has_more);
        assert_eq!(page.next_before_seq, Some(3));
    }

    #[test]
    fn query_filters_by_user_and_contains() {
        let buffer = sample_buffer();
        let page = buffer.query(ChatHistoryQuery {
            limit: Some(10),
            user: Some("ALICE".into()),
            contains: Some("WEATHER".into()),
            before_seq: None,
        });

        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].seq, 3);
        assert_eq!(page.messages[0].text, "weather got better");
        assert!(!page.has_more);
    }

    #[test]
    fn query_pages_with_before_seq() {
        let buffer = sample_buffer();
        let page = buffer.query(ChatHistoryQuery {
            limit: Some(2),
            before_seq: Some(4),
            ..ChatHistoryQuery::default()
        });

        assert_eq!(
            page.messages
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(page.has_more);
        assert_eq!(page.next_before_seq, Some(2));
    }

    #[test]
    fn query_clamps_limit_to_tool_maximum() {
        let mut buffer = ChatHistoryBuffer::new(250);
        for i in 0..250 {
            buffer.push_user("alice", format!("message {i}"));
        }

        let page = buffer.query(ChatHistoryQuery {
            limit: Some(500),
            ..ChatHistoryQuery::default()
        });

        assert_eq!(page.messages.len(), MAX_TOOL_RESULT_MESSAGES);
        assert!(page.has_more);
        assert_eq!(page.messages[0].seq, 51);
    }

    #[test]
    fn from_prefill_assigns_sequence_numbers_and_marks_user_source() {
        let buffer = ChatHistoryBuffer::from_prefill(
            2,
            vec![
                ("alice".to_string(), "one".to_string(), Utc::now()),
                ("bob".to_string(), "two".to_string(), Utc::now()),
                ("carol".to_string(), "three".to_string(), Utc::now()),
            ],
        );

        let page = buffer.query(ChatHistoryQuery {
            limit: Some(10),
            ..ChatHistoryQuery::default()
        });
        assert_eq!(page.messages.len(), 2);
        assert_eq!(page.messages[0].seq, 1);
        assert_eq!(page.messages[0].username, "bob");
        assert_eq!(page.messages[1].seq, 2);
        assert_eq!(page.messages[1].source, ChatHistorySource::User);
    }
}
