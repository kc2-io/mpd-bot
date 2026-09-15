//! Atomic, app-owned API profile storage. Snapshots never contain keys.
use crate::{
    config::{Config, Provider, validate_endpoint},
    credential_files,
    secrets::Secrets,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

const MAX_PROFILES: usize = 16;
const MAX_STORE_BYTES: usize = 64 * 1024;
const FILE: &str = "ai-profiles.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiProfile {
    pub id: String,
    pub name: String,
    pub provider: Provider,
    pub model: String,
    pub endpoint: String,
}
impl ApiProfile {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 40
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("Invalid API profile identifier.".into());
        }
        if self.name.trim().is_empty()
            || self.name.len() > 80
            || self.name.chars().any(char::is_control)
        {
            return Err("Profile name must be 1–80 bytes without control characters.".into());
        }
        if self.model.len() > 200 || self.model.chars().any(char::is_control) {
            return Err("Invalid profile model ID.".into());
        }
        if self.endpoint.len() > 2048 {
            return Err("Profile endpoint is too long.".into());
        }
        if !self.endpoint.is_empty() {
            validate_endpoint(&self.endpoint)?;
        }
        Ok(())
    }
    pub fn apply(&self, config: &mut Config) {
        config.provider = self.provider;
        config.model = self.model.clone();
        config.endpoint = self.endpoint.clone();
    }
}
#[derive(Clone)]
pub struct ProfileSummary {
    pub profile: ApiProfile,
    pub has_key: bool,
}
// Intentionally no Debug implementation: the serialized store contains keys.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    profile: ApiProfile,
    key: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    version: u32,
    active_id: String,
    records: Vec<Record>,
}

impl Profiles {
    pub fn has_file(directory: &Path) -> bool {
        directory
            .join("credentials")
            .join(FILE)
            .try_exists()
            .unwrap_or(true)
    }
    pub fn from_legacy(config: &Config, keys: &Secrets) -> Self {
        let mut records = Vec::new();
        for provider in Provider::ALL {
            let key = keys.get(provider.id());
            if provider != config.provider && key.is_empty() {
                continue;
            }
            let active = provider == config.provider;
            records.push(Record {
                profile: ApiProfile {
                    id: format!("imported-{}", provider.id()),
                    name: match provider {
                        Provider::Openai => "OpenAI",
                        Provider::Anthropic => "Anthropic",
                        Provider::Openrouter => "OpenRouter",
                        Provider::Compatible => "Compatible",
                    }
                    .into(),
                    provider,
                    model: if active {
                        config.model.clone()
                    } else {
                        String::new()
                    },
                    endpoint: if active && provider == Provider::Compatible {
                        config.endpoint.clone()
                    } else {
                        String::new()
                    },
                },
                key,
            });
        }
        Self {
            version: 1,
            active_id: format!("imported-{}", config.provider.id()),
            records,
        }
    }
    pub fn load_or_migrate(
        directory: &Path,
        config: &Config,
        keys: &Secrets,
    ) -> Result<Self, String> {
        let path = directory.join("credentials").join(FILE);
        if let Some(bytes) = credential_files::read(&path)
            .map_err(|_| "Could not read API profiles. Check credential-folder permissions.")?
        {
            let store: Self = serde_json::from_slice(&bytes).map_err(|_| "The API profile file is invalid. Restore it from a backup before editing profiles.")?;
            store.validate()?;
            return Ok(store);
        }
        if keys.warning.is_some() {
            return Err("Could not import existing API keys. Check credential files before creating profiles.".into());
        }
        let store = Self::from_legacy(config, keys);
        store.persist(directory)?;
        Ok(store)
    }
    pub fn active_id(&self) -> &str {
        &self.active_id
    }
    pub fn active(&self) -> &ApiProfile {
        &self
            .records
            .iter()
            .find(|r| r.profile.id == self.active_id)
            .expect("validated active profile")
            .profile
    }
    pub fn key(&self) -> String {
        self.records
            .iter()
            .find(|r| r.profile.id == self.active_id)
            .expect("validated active profile")
            .key
            .clone()
    }
    pub fn summaries(&self) -> Vec<ProfileSummary> {
        self.records
            .iter()
            .map(|r| ProfileSummary {
                profile: r.profile.clone(),
                has_key: !r.key.is_empty(),
            })
            .collect()
    }
    pub fn save(&mut self, mut profile: ApiProfile, key: Option<String>) -> Result<(), String> {
        profile.name = profile.name.trim().into();
        profile.model = profile.model.trim().into();
        profile.endpoint = profile.endpoint.trim().into();
        if profile.provider != Provider::Compatible {
            profile.endpoint.clear();
        }
        if profile.id.is_empty() {
            profile.id = format!("p-{:032x}", fastrand::u128(..));
        }
        profile.validate()?;
        if self.records.iter().any(|r| {
            r.profile.id != profile.id
                && r.profile.name.to_lowercase() == profile.name.to_lowercase()
        }) {
            return Err("Choose a different profile name; that name is already saved.".into());
        }
        let supplied = key.map(|k| k.trim().to_owned());
        if let Some(key) = &supplied
            && !key.is_empty()
            && (key.len() > 4096 || key.chars().any(char::is_control))
        {
            return Err("Enter a valid API key.".into());
        }
        if let Some(record) = self.records.iter_mut().find(|r| r.profile.id == profile.id) {
            let same_destination = record.profile.provider == profile.provider
                && record.profile.endpoint == profile.endpoint;
            record.key = supplied.unwrap_or_else(|| {
                if same_destination {
                    record.key.clone()
                } else {
                    String::new()
                }
            });
            record.profile = profile.clone();
        } else {
            if self.records.len() >= MAX_PROFILES {
                return Err("You can save up to 16 API profiles.".into());
            }
            self.records.push(Record {
                profile: profile.clone(),
                key: supplied.unwrap_or_default(),
            });
        }
        self.active_id = profile.id;
        self.validate()
    }
    pub fn activate(&mut self, id: &str) -> Result<(), String> {
        if !self.records.iter().any(|r| r.profile.id == id) {
            return Err("API profile no longer exists. Refresh and try again.".into());
        }
        self.active_id = id.into();
        Ok(())
    }
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        if self.records.len() <= 1 {
            return Err(
                "Keep at least one API profile. You can rename it or remove its key.".into(),
            );
        }
        if !self.records.iter().any(|r| r.profile.id == id) {
            return Err("API profile no longer exists.".into());
        }
        self.records.retain(|r| r.profile.id != id);
        if self.active_id == id {
            self.active_id = self.records[0].profile.id.clone();
        }
        Ok(())
    }
    pub fn remove_key(&mut self, id: &str) -> Result<(), String> {
        let record = self
            .records
            .iter_mut()
            .find(|r| r.profile.id == id)
            .ok_or("API profile no longer exists.")?;
        record.key.clear();
        Ok(())
    }
    fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.records.is_empty()
            || self.records.len() > MAX_PROFILES
            || !self.records.iter().any(|r| r.profile.id == self.active_id)
        {
            return Err(
                "Invalid API profile store. Restore it from a backup before editing profiles."
                    .into(),
            );
        }
        let mut names = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for record in &self.records {
            record.profile.validate()?;
            if !names.insert(record.profile.name.trim().to_lowercase())
                || !ids.insert(&record.profile.id)
                || record.key.len() > 4096
                || record.key.chars().any(char::is_control)
            {
                return Err("Invalid API profile record.".into());
            }
        }
        Ok(())
    }
    pub fn persist(&self, directory: &Path) -> Result<(), String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| "Could not encode API profiles.")?;
        if bytes.len() > MAX_STORE_BYTES {
            return Err("Saved API profiles exceed the 64 KiB storage limit.".into());
        }
        credential_files::write(&directory.join("credentials").join(FILE), &bytes)
            .map_err(|_| "Could not save API profiles. Check credential-folder permissions.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{diagnostics::SecretValue, secrets::Secret};
    fn keys() -> Secrets {
        Secrets {
            values: Default::default(),
            warning: None,
        }
    }
    fn draft(name: &str, provider: Provider, model: &str) -> ApiProfile {
        ApiProfile {
            id: String::new(),
            name: name.into(),
            provider,
            model: model.into(),
            endpoint: String::new(),
        }
    }
    #[test]
    fn independent_keys_models_names_and_active_selection_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Profiles::from_legacy(&Config::default(), &keys());
        store
            .save(
                draft("Fast", Provider::Openai, "fast-model"),
                Some("synthetic-key-a".into()),
            )
            .unwrap();
        let first = store.active_id().to_owned();
        store
            .save(
                draft("Creative", Provider::Openai, "creative-model"),
                Some("synthetic-key-b".into()),
            )
            .unwrap();
        let second = store.active_id().to_owned();
        store
            .save(
                draft("Claude", Provider::Anthropic, "claude-model"),
                Some("synthetic-key-c".into()),
            )
            .unwrap();
        let third = store.active_id().to_owned();
        store.persist(dir.path()).unwrap();
        let mut restored =
            Profiles::load_or_migrate(dir.path(), &Config::default(), &keys()).unwrap();
        assert_eq!(restored.active_id(), third);
        for (id, name, model, key) in [
            (first, "Fast", "fast-model", "synthetic-key-a"),
            (second, "Creative", "creative-model", "synthetic-key-b"),
            (third, "Claude", "claude-model", "synthetic-key-c"),
        ] {
            restored.activate(&id).unwrap();
            assert_eq!(restored.active().name, name);
            assert_eq!(restored.active().model, model);
            assert_eq!(restored.key(), key);
        }
        assert!(!dir.path().join("config.json").exists());
        let safe = restored
            .summaries()
            .into_iter()
            .map(|p| format!("{:?}", p.profile))
            .collect::<String>();
        assert!(!safe.contains("synthetic-key"));
    }
    #[test]
    fn rename_retains_key_and_destination_change_requires_replacement() {
        let mut store = Profiles::from_legacy(&Config::default(), &keys());
        store
            .save(
                draft("Local", Provider::Compatible, "one"),
                Some("synthetic-original".into()),
            )
            .unwrap();
        let mut changed = store.active().clone();
        changed.name = "Renamed".into();
        changed.model = "two".into();
        store.save(changed.clone(), None).unwrap();
        assert_eq!(store.key(), "synthetic-original");
        changed.endpoint = "https://example.com/v1/chat/completions".into();
        store.save(changed.clone(), None).unwrap();
        assert_eq!(store.key(), "");
        store
            .save(changed.clone(), Some("synthetic-replacement".into()))
            .unwrap();
        changed.provider = Provider::Anthropic;
        store.save(changed, None).unwrap();
        assert_eq!(store.key(), "");
        assert!(store.active().endpoint.is_empty());
    }
    #[test]
    fn migration_copies_legacy_keys_once_and_preserves_selected_settings() {
        let dir = tempfile::tempdir().unwrap();
        let mut legacy = keys();
        legacy.values.insert(
            "anthropic".into(),
            Secret {
                value: SecretValue::new("synthetic-claude".into()),
            },
        );
        legacy.values.insert(
            "openai".into(),
            Secret {
                value: SecretValue::new("synthetic-openai".into()),
            },
        );
        let config = Config {
            provider: Provider::Anthropic,
            model: "existing-model".into(),
            ..Config::default()
        };
        let mut store = Profiles::load_or_migrate(dir.path(), &config, &legacy).unwrap();
        assert_eq!(store.active().model, "existing-model");
        assert_eq!(store.key(), "synthetic-claude");
        let id = store.active_id().to_owned();
        store.remove_key(&id).unwrap();
        store.persist(dir.path()).unwrap();
        let restored = Profiles::load_or_migrate(dir.path(), &config, &legacy).unwrap();
        assert!(restored.key().is_empty());
        assert_eq!(restored.summaries().len(), 2);
    }
    #[test]
    fn validation_deletion_and_corrupt_storage_do_not_expose_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Profiles::from_legacy(&Config::default(), &keys());
        let id = store.active_id().to_owned();
        assert!(store.delete(&id).is_err());
        assert!(
            store
                .save(draft("openai", Provider::Openai, "model"), None)
                .is_err()
        );
        let mut bad = draft("Good", Provider::Openai, "model");
        bad.id = "../escape".into();
        assert!(store.save(bad, None).is_err());
        assert!(store.activate("missing").is_err());
        store
            .save(
                draft("Second", Provider::Openrouter, "model"),
                Some("synthetic-private".into()),
            )
            .unwrap();
        let second = store.active_id().to_owned();
        store.delete(&second).unwrap();
        assert_eq!(store.active_id(), id);
        store.persist(dir.path()).unwrap();
        let file = dir.path().join("credentials").join(FILE);
        assert!(
            !std::fs::read_to_string(&file)
                .unwrap()
                .contains("synthetic-private")
        );
        let corrupt = br#"{"version":9,"key":"synthetic-secret"}"#;
        credential_files::write(&file, corrupt).unwrap();
        let error = Profiles::load_or_migrate(dir.path(), &Config::default(), &keys())
            .err()
            .unwrap();
        assert!(!error.contains("synthetic-secret"));
        assert_eq!(std::fs::read(file).unwrap(), corrupt);
    }
    #[test]
    fn profile_count_is_bounded() {
        let mut store = Profiles::from_legacy(&Config::default(), &keys());
        for i in 1..MAX_PROFILES {
            store
                .save(
                    draft(&format!("Profile {i}"), Provider::Openai, "model"),
                    None,
                )
                .unwrap();
        }
        assert!(
            store
                .save(draft("One too many", Provider::Openai, "model"), None)
                .is_err()
        );
        assert_eq!(store.summaries().len(), MAX_PROFILES);
    }
}
