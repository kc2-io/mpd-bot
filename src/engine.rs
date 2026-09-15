use crate::{config::Config, provider::{self, Message, Role}};
use serde::{Deserialize, Serialize};
use std::{collections::{HashMap, VecDeque}, time::Instant};

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ChatInput { pub platform: String, pub channel: String, pub user: String, pub message: String }

impl ChatInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.message.trim().is_empty() || self.message.len() > 4000 { return Err("Message must be 1–4000 bytes.".into()); }
        for id in [&self.platform, &self.channel, &self.user] {
            if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) { return Err("Invalid platform, channel, or user.".into()); }
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub struct ChatOutput { pub reply: Option<String>, pub skipped: Option<&'static str> }
impl ChatOutput {
    pub fn skip(reason: &'static str) -> Self { Self { reply: None, skipped: Some(reason) } }
}

type ConversationId = (String, String, String);
#[derive(Default)]
struct Conversation { messages: VecDeque<Message>, touched: u64 }
#[derive(Default)]
pub struct Engine {
    conversations: HashMap<ConversationId, Conversation>,
    sequence: u64,
    last_attempt: Option<Instant>,
    pub replies: u64,
    pub last_error: Option<String>,
}

impl Engine {
    pub fn count(&self) -> usize { self.conversations.len() }
    pub fn clear(&mut self) { self.conversations.clear(); }
    pub async fn chat(&mut self, client: &reqwest::Client, config: &Config, key: &str, input: ChatInput, preview: bool) -> Result<ChatOutput, String> {
        input.validate()?;
        if !preview {
            if !config.enabled { return Ok(ChatOutput::skip("paused")); }
            if config.excluded_users.iter().any(|u| u.eq_ignore_ascii_case(&input.user)) { return Ok(ChatOutput::skip("excluded")); }
            // Three seconds also keeps direct Twitch sends below the ordinary 20/30s limit.
            if self.last_attempt.is_some_and(|t| t.elapsed().as_secs() < config.cooldown_seconds.max(3)) { return Ok(ChatOutput::skip("cooldown")); }
        }
        if config.model.trim().is_empty() { return Err("Choose a model ID in configuration first.".into()); }
        if key.is_empty() && config.provider != crate::config::Provider::Compatible { return Err("Add an API key for the selected provider.".into()); }
        let id = (input.platform.to_lowercase(), input.channel.to_lowercase(), input.user.to_lowercase());
        let mut messages: Vec<Message> = if preview { Vec::new() } else { self.conversations.get(&id).map(|c| c.messages.iter().cloned().collect()).unwrap_or_default() };
        let user_message = Message { role: Role::User, content: input.message };
        messages.push(user_message.clone());
        if !preview { self.last_attempt = Some(Instant::now()); }
        let response = provider::complete(client, config, key, &messages).await;
        let reply = match response {
            Ok(text) => clean_reply(&text, config.max_reply_chars),
            Err(error) => { self.last_error = Some(error.clone()); return Err(error); }
        };
        if reply.is_empty() { return Err("Provider returned an empty reply.".into()); }
        if !preview {
            self.remember(config, id, user_message, reply.clone());
            self.replies += 1;
            self.last_error = None;
        }
        Ok(ChatOutput { reply: Some(reply), skipped: None })
    }
    fn remember(&mut self, config: &Config, id: ConversationId, user: Message, reply: String) {
        if config.memory_turns == 0 { self.clear(); return; }
        if !self.conversations.contains_key(&id) && self.conversations.len() >= config.max_conversations {
            if let Some(oldest) = self.conversations.iter().min_by_key(|(_, c)|c.touched).map(|(k,_)|k.clone()) { self.conversations.remove(&oldest); }
        }
        self.sequence += 1;
        let c = self.conversations.entry(id).or_default();
        c.touched = self.sequence;
        c.messages.push_back(user);
        c.messages.push_back(Message { role: Role::Assistant, content: reply });
        while c.messages.len() > config.memory_turns * 2 { c.messages.pop_front(); }
    }
}

pub fn clean_reply(text: &str, limit: usize) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().filter(|c| !c.is_control()).take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn bounded_memory_evicts_oldest_and_keeps_complete_turns() {
        let mut e = Engine::default();
        let c = Config { memory_turns:1, max_conversations:2, ..Config::default() };
        let id = |u: &str| ("twitch".into(), "channel".into(), u.into());
        for u in ["a", "b", "a", "c"] { e.remember(&c, id(u), Message { role:Role::User, content:"question".into() }, "answer".into()); }
        assert_eq!(e.count(), 2); assert!(!e.conversations.contains_key(&id("b"))); assert_eq!(e.conversations[&id("a")].messages.len(), 2);
        e.remember(&Config { memory_turns:0, ..c }, id("z"), Message { role:Role::User, content:"question".into() }, "answer".into());
        assert_eq!(e.count(), 0);
    }
    #[test] fn unicode_and_newlines_are_safe() { assert_eq!(clean_reply(" Hi\r\n世界 😀 extra", 7), "Hi 世界 😀"); }
}
