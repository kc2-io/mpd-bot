use crate::{
    config::Config,
    provider::{Message, Role},
};
use serde::Deserialize;
use std::{
    collections::{HashMap, VecDeque},
    time::Instant,
};

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ChatInput {
    pub platform: String,
    pub channel: String,
    pub user: String,
    pub message: String,
}
impl ChatInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.message.trim().is_empty() || self.message.len() > MAX_INPUT_BYTES {
            return Err("Message must be 1–4000 bytes.".into());
        }
        if [&self.platform, &self.channel, &self.user]
            .iter()
            .any(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
        {
            return Err("Invalid chat identity.".into());
        }
        Ok(())
    }
}
pub const CHAT_TEXT_BUDGET: usize = 2 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 4000;

/// Full-history planning estimate, not total process RAM. Include maximum UTF-8
/// text plus an approximate allowance for keys, messages and collection capacity.
pub fn estimated_chat_memory(messages: usize, viewers: usize, reply_chars: usize) -> usize {
    let messages = messages.min(20);
    let viewers = viewers.min(128);
    if messages == 0 {
        return 0;
    }
    let text =
        (messages * viewers * (MAX_INPUT_BYTES + reply_chars.min(450) * 4)).min(CHAT_TEXT_BUDGET);
    text + viewers * (512 + messages * 2 * 128)
}

type ConversationId = (String, String, String);
pub struct PreparedTurn {
    id: ConversationId,
    pub messages: Vec<Message>,
    user: Message,
    preview: bool,
}
#[derive(Default)]
struct Conversation {
    messages: VecDeque<Message>,
    touched: u64,
}
#[derive(Default)]
pub struct Engine {
    conversations: HashMap<ConversationId, Conversation>,
    sequence: u64,
    last_attempt: Option<Instant>,
    pub replies: u64,
}
impl Engine {
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.conversations.len()
    }
    pub fn clear(&mut self) {
        self.conversations.clear();
    }
    /// Prepare under a short lock. This never calls a network service or commits memory.
    pub fn prepare(
        &mut self,
        config: &Config,
        input: ChatInput,
        preview: bool,
    ) -> Result<Option<PreparedTurn>, String> {
        input.validate()?;
        if !preview {
            if !config.enabled {
                return Ok(None);
            }
            if self
                .last_attempt
                .is_some_and(|t| t.elapsed().as_secs() < config.cooldown_seconds.max(3))
            {
                return Ok(None);
            }
        }
        if config.model.trim().is_empty() {
            return Err("Choose a model ID first.".into());
        }
        let id = (
            input.platform.to_lowercase(),
            input.channel.to_lowercase(),
            input.user.to_lowercase(),
        );
        let mut messages = if preview {
            Vec::new()
        } else {
            self.conversations
                .get(&id)
                .map(|c| c.messages.iter().cloned().collect())
                .unwrap_or_default()
        };
        let user = Message {
            role: Role::User,
            content: input.message,
        };
        messages.push(user.clone());
        if !preview {
            self.last_attempt = Some(Instant::now());
        }
        Ok(Some(PreparedTurn {
            id,
            messages,
            user,
            preview,
        }))
    }
    /// Only the transport's confirmed delivery path may call this.
    pub fn delivered(&mut self, config: &Config, turn: PreparedTurn, reply: String) {
        if turn.preview {
            return;
        }
        self.replies += 1;
        self.remember(config, turn.id, turn.user, reply);
    }
    fn remember(&mut self, config: &Config, id: ConversationId, user: Message, reply: String) {
        if config.memory_turns == 0 {
            self.clear();
            return;
        }
        if !self.conversations.contains_key(&id)
            && self.conversations.len() >= config.max_conversations
        {
            self.evict();
        }
        self.sequence = self.sequence.wrapping_add(1);
        let conversation = self.conversations.entry(id).or_default();
        conversation.touched = self.sequence;
        conversation.messages.push_back(user);
        conversation.messages.push_back(Message {
            role: Role::Assistant,
            content: reply,
        });
        while conversation.messages.len() > config.memory_turns * 2 {
            conversation.messages.pop_front();
        }
        while self.bytes() > CHAT_TEXT_BUDGET {
            self.evict();
        }
    }
    fn evict(&mut self) {
        if let Some(oldest) = self
            .conversations
            .iter()
            .min_by_key(|(_, c)| c.touched)
            .map(|(k, _)| k.clone())
        {
            self.conversations.remove(&oldest);
        }
    }
    fn bytes(&self) -> usize {
        self.conversations
            .values()
            .flat_map(|c| c.messages.iter())
            .map(|m| m.content.len())
            .sum()
    }
}
pub fn clean_reply(text: &str, limit: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn input(user: &str) -> ChatInput {
        ChatInput {
            platform: "twitch".into(),
            channel: "channel".into(),
            user: user.into(),
            message: "hello".into(),
        }
    }
    #[test]
    fn chat_memory_estimate_tracks_settings_and_text_cap() {
        assert_eq!(estimated_chat_memory(0, 128, 450), 0);
        assert!(estimated_chat_memory(2, 16, 100) > estimated_chat_memory(1, 16, 100));
        assert!(estimated_chat_memory(2, 32, 100) > estimated_chat_memory(2, 16, 100));
        assert!(estimated_chat_memory(2, 16, 450) > estimated_chat_memory(2, 16, 100));
        let full = estimated_chat_memory(20, 128, 450);
        assert_eq!(full, CHAT_TEXT_BUDGET + 128 * (512 + 20 * 2 * 128));
        assert!(full < 3 * 1024 * 1024);
    }
    #[test]
    fn live_replies_obey_pause_and_cooldown_while_preview_is_isolated() {
        let mut engine = Engine::default();
        let mut config = Config {
            model: "test".into(),
            cooldown_seconds: 0,
            ..Config::default()
        };
        config.enabled = false;
        assert!(
            engine
                .prepare(&config, input("a"), false)
                .unwrap()
                .is_none()
        );
        config.enabled = true;
        assert!(
            engine
                .prepare(&config, input("a"), false)
                .unwrap()
                .is_some()
        );
        assert!(
            engine
                .prepare(&config, input("b"), false)
                .unwrap()
                .is_none()
        );
        assert!(engine.prepare(&config, input("a"), true).unwrap().is_some());
        engine.last_attempt = Some(Instant::now() - std::time::Duration::from_secs(3));
        assert!(
            engine
                .prepare(&config, input("b"), false)
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn only_confirmed_delivery_commits() {
        let mut engine = Engine::default();
        let config = Config {
            model: "test".into(),
            ..Config::default()
        };
        let turn = engine
            .prepare(&config, input("viewer"), false)
            .unwrap()
            .unwrap();
        assert_eq!(engine.count(), 0); // Failed/unknown delivery just drops the prepared turn.
        drop(turn);
        assert_eq!(engine.replies, 0);
        let preview = engine
            .prepare(&config, input("viewer"), true)
            .unwrap()
            .unwrap();
        engine.delivered(&config, preview, "private".into());
        assert_eq!(engine.count(), 0);
        engine.last_attempt = None;
        let delivered = engine
            .prepare(&config, input("viewer"), false)
            .unwrap()
            .unwrap();
        engine.delivered(&config, delivered, "sent".into());
        assert_eq!(engine.count(), 1);
        assert_eq!(engine.replies, 1);
    }
    #[test]
    fn bounded_memory_evicts_oldest() {
        let mut e = Engine::default();
        let c = Config {
            memory_turns: 1,
            max_conversations: 2,
            ..Config::default()
        };
        let id = |u: &str| ("twitch".into(), "channel".into(), u.into());
        for u in ["a", "b", "a", "c"] {
            e.remember(
                &c,
                id(u),
                Message {
                    role: Role::User,
                    content: "question".into(),
                },
                "answer".into(),
            );
        }
        assert_eq!(e.count(), 2);
        assert!(!e.conversations.contains_key(&id("b")));
        assert_eq!(e.conversations[&id("a")].messages.len(), 2);
        e.remember(
            &Config {
                memory_turns: 0,
                ..c
            },
            id("z"),
            Message {
                role: Role::User,
                content: "question".into(),
            },
            "answer".into(),
        );
        assert_eq!(e.count(), 0);
    }
    #[test]
    fn total_memory_has_byte_budget() {
        let mut e = Engine::default();
        let c = Config {
            memory_turns: 20,
            max_conversations: 128,
            ..Config::default()
        };
        for i in 0..900 {
            e.remember(
                &c,
                ("twitch".into(), "channel".into(), (i % 128).to_string()),
                Message {
                    role: Role::User,
                    content: "a".repeat(4000),
                },
                "b".repeat(1800),
            );
        }
        assert!(e.bytes() <= 2 * 1024 * 1024);
    }
    #[test]
    fn unicode_newlines() {
        assert_eq!(clean_reply(" Hi\r\n世界 😀 extra", 7), "Hi 世界 😀");
    }
}
