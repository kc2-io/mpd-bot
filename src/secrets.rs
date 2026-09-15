use crate::{config::Provider, credential_files, diagnostics::SecretValue};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub const IDS: [&str; 5] = ["openai", "anthropic", "openrouter", "compatible", "twitch"];
pub struct Secret {
    pub value: SecretValue,
    pub source: &'static str,
}
pub struct Secrets {
    pub values: HashMap<String, Secret>,
    pub warning: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredKey {
    version: u32,
    provider: String,
    key: String,
}

fn path(directory: &Path, id: &str) -> Result<PathBuf, String> {
    if !Provider::ALL.iter().any(|provider| provider.id() == id) {
        return Err("Unknown provider credential.".into());
    }
    Ok(directory
        .join("credentials")
        .join(format!("provider-{id}.json")))
}
fn load_saved(directory: &Path, id: &str) -> Result<Option<String>, String> {
    let Some(bytes) = credential_files::read(&path(directory, id)?)
        .map_err(|_| "Could not read the saved API key. Check credential-file permissions.")?
    else {
        return Ok(None);
    };
    let saved: StoredKey =
        serde_json::from_slice(&bytes).map_err(|_| "The saved API key file is invalid.")?;
    if saved.version != 1 || saved.provider != id || !valid_key(&saved.key) {
        return Err("The saved API key file is invalid.".into());
    }
    Ok(Some(saved.key))
}
fn valid_key(key: &str) -> bool {
    !key.trim().is_empty() && key.len() <= 4096 && !key.chars().any(char::is_control)
}
impl Secrets {
    pub fn load(directory: &Path) -> Self {
        let mut values = HashMap::new();
        let mut warning = None;
        for id in IDS {
            let env = if id == "twitch" {
                "TWITCH_ACCESS_TOKEN"
            } else {
                Provider::ALL.iter().find(|p| p.id() == id).unwrap().env()
            };
            if let Ok(value) = std::env::var(env)
                && valid_key(value.trim())
            {
                values.insert(
                    id.into(),
                    Secret {
                        value: SecretValue::new(value.trim().into()),
                        source: "environment",
                    },
                );
                continue;
            }
            // Access-only Twitch compatibility uses the explicit environment token.
            // Refreshable Twitch login has its own versioned credential file.
            if id == "twitch" {
                continue;
            }
            match load_saved(directory, id) {
                Ok(Some(value)) => {
                    values.insert(
                        id.into(),
                        Secret {
                            value: SecretValue::new(value),
                            source: "file",
                        },
                    );
                }
                Ok(None) => {}
                Err(error) => warning = Some(error),
            }
        }
        Self { values, warning }
    }
    pub fn get(&self, id: &str) -> String {
        self.values
            .get(id)
            .map(|s| s.value.expose().to_string())
            .unwrap_or_default()
    }
    pub fn status(&self) -> serde_json::Value {
        let map: serde_json::Map<String, serde_json::Value> = IDS.into_iter().map(|id| (id.into(), serde_json::json!({"configured":self.values.contains_key(id), "source":self.values.get(id).map(|s|s.source)}))).collect();
        serde_json::Value::Object(map)
    }
}
pub fn persist(directory: &Path, id: &str, value: &str) -> Result<(), String> {
    if !valid_key(value) {
        return Err("Enter a valid API key.".into());
    }
    let file = path(directory, id)?;
    let bytes = serde_json::to_vec(&StoredKey {
        version: 1,
        provider: id.into(),
        key: value.into(),
    })
    .map_err(|_| "Could not encode the API key.")?;
    credential_files::write(&file, &bytes)
        .map_err(|_| "Could not save the API key. Check credential-folder permissions.".into())
}
pub fn forget(directory: &Path, id: &str) -> Result<(), String> {
    credential_files::delete(&path(directory, id)?).map_err(|_| {
        "Could not remove the saved API key. Check credential-folder permissions.".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_restore_rotate_and_remove_without_using_settings_file() {
        let directory = tempfile::tempdir().unwrap();
        persist(directory.path(), "anthropic", "synthetic-first-key").unwrap();
        assert_eq!(
            load_saved(directory.path(), "anthropic")
                .unwrap()
                .as_deref(),
            Some("synthetic-first-key")
        );
        persist(directory.path(), "anthropic", "synthetic-replacement-key").unwrap();
        assert_eq!(
            load_saved(directory.path(), "anthropic")
                .unwrap()
                .as_deref(),
            Some("synthetic-replacement-key")
        );
        assert!(load_saved(directory.path(), "openai").unwrap().is_none());
        assert!(!directory.path().join("config.json").exists());
        forget(directory.path(), "anthropic").unwrap();
        assert!(load_saved(directory.path(), "anthropic").unwrap().is_none());
        assert!(persist(directory.path(), "../outside", "synthetic").is_err());
    }
    #[test]
    fn invalid_saved_key_is_reported_without_returning_file_contents() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path(), "openai").unwrap();
        credential_files::write(
            &file,
            br#"{"version":2,"provider":"openai","key":"private-sentinel"}"#,
        )
        .unwrap();
        let error = load_saved(directory.path(), "openai").unwrap_err();
        assert!(!error.contains("private-sentinel"));
        assert!(error.contains("invalid"));
    }
}
