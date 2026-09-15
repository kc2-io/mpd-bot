use crate::config::Provider;
use std::collections::HashMap;

pub const IDS: [&str; 5] = ["openai", "anthropic", "openrouter", "compatible", "twitch"];
pub struct Secret { pub value: String, pub source: &'static str }
pub struct Secrets { pub values: HashMap<String, Secret>, pub warning: Option<String> }

impl Secrets {
    pub fn load() -> Self {
        let mut values = HashMap::new();
        let mut warning = None;
        for id in IDS {
            let env = if id == "twitch" { "TWITCH_ACCESS_TOKEN" } else { Provider::ALL.iter().find(|p| p.id() == id).unwrap().env() };
            if let Ok(value) = std::env::var(env) {
                if !value.trim().is_empty() { values.insert(id.into(), Secret { value: value.trim().into(), source: "environment" }); continue; }
            }
            match keyring::Entry::new("io.kc2.mpd-bot", id).and_then(|e| e.get_password()) {
                Ok(value) => { values.insert(id.into(), Secret { value, source: "keychain" }); }
                Err(keyring::Error::NoEntry) => {},
                Err(_) => { warning = Some("OS credential storage is unavailable or locked. You can still use session-only keys or environment variables.".into()); }
            }
        }
        Self { values, warning }
    }
    pub fn get(&self, id: &str) -> String { self.values.get(id).map(|s| s.value.clone()).unwrap_or_default() }
    pub fn status(&self) -> serde_json::Value {
        let map: serde_json::Map<String, serde_json::Value> = IDS.into_iter().map(|id| (id.into(), serde_json::json!({"configured":self.values.contains_key(id), "source":self.values.get(id).map(|s|s.source)}))).collect();
        serde_json::Value::Object(map)
    }
}

pub fn persist(id: &str, value: &str) -> Result<(), String> {
    keyring::Entry::new("io.kc2.mpd-bot", id).and_then(|e| e.set_password(value))
        .map_err(|_| "Could not save to the OS credential store. Unlock it or choose session-only storage.".into())
}
pub fn forget(id: &str) -> Result<(), String> {
    match keyring::Entry::new("io.kc2.mpd-bot", id).and_then(|e| e.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("Could not remove the saved credential. Unlock the OS credential store and try again.".into())
    }
}
