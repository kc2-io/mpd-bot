//! OAuth credentials are a single versioned record in an app-owned credential file.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct CredentialBundle {
    pub version: u32,
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub user_id: String,
    pub login: String,
    pub scopes: Vec<String>,
}

pub(super) trait CredentialStore: Send + Sync {
    fn load(&self, client_id: &str) -> Result<Option<CredentialBundle>, ()>;
    fn save(&self, bundle: &CredentialBundle) -> Result<(), ()>;
    fn delete(&self, client_id: &str) -> Result<(), ()>;
}

pub(super) struct FileCredentialStore {
    directory: PathBuf,
}

impl FileCredentialStore {
    pub fn new(config_directory: PathBuf) -> Self {
        Self {
            directory: config_directory.join("credentials"),
        }
    }

    fn path(&self, client_id: &str) -> Result<PathBuf, ()> {
        // Client IDs are opaque identifiers, never path components supplied verbatim.
        if client_id.is_empty()
            || client_id.len() > 128
            || !client_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(());
        }
        Ok(self.directory.join(format!("twitch-{client_id}.json")))
    }
}

fn valid_bundle(bundle: &CredentialBundle, client_id: &str) -> bool {
    bundle.version == 1
        && bundle.client_id == client_id
        && !bundle.access_token.is_empty()
        && !bundle.refresh_token.is_empty()
        && !bundle.user_id.is_empty()
        && !bundle.login.is_empty()
}

impl CredentialStore for FileCredentialStore {
    fn load(&self, client_id: &str) -> Result<Option<CredentialBundle>, ()> {
        let Some(bytes) = crate::credential_files::read(&self.path(client_id)?)? else {
            return Ok(None);
        };
        if bytes.len() > 262_144 {
            return Err(());
        }
        let bundle: CredentialBundle = serde_json::from_slice(&bytes).map_err(|_| ())?;
        if !valid_bundle(&bundle, client_id) {
            return Err(());
        }
        Ok(Some(bundle))
    }

    fn save(&self, bundle: &CredentialBundle) -> Result<(), ()> {
        let path = self.path(&bundle.client_id)?;
        if !valid_bundle(bundle, &bundle.client_id) {
            return Err(());
        }
        let bytes = serde_json::to_vec(bundle).map_err(|_| ())?;
        if bytes.len() > 262_144 {
            return Err(());
        }
        crate::credential_files::write(&path, &bytes)
    }

    fn delete(&self, client_id: &str) -> Result<(), ()> {
        crate::credential_files::delete(&self.path(client_id)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(client_id: &str) -> CredentialBundle {
        CredentialBundle {
            version: 1,
            client_id: client_id.into(),
            access_token: "synthetic-access".into(),
            refresh_token: "synthetic-refresh".into(),
            expires_at: 123,
            user_id: "123".into(),
            login: "testbot".into(),
            scopes: vec!["user:read:chat".into(), "user:write:chat".into()],
        }
    }

    #[test]
    fn persists_rotated_pair_across_store_instances_and_deletes() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(directory.path().into());
        assert!(store.load("client1").unwrap().is_none());
        store.save(&bundle("client1")).unwrap();
        let restored = FileCredentialStore::new(directory.path().into());
        let mut rotated = restored.load("client1").unwrap().unwrap();
        assert_eq!(rotated.access_token, "synthetic-access");
        rotated.access_token = "rotated-access".into();
        rotated.refresh_token = "rotated-refresh".into();
        restored.save(&rotated).unwrap();
        let latest = store.load("client1").unwrap().unwrap();
        assert_eq!(latest.access_token, "rotated-access");
        assert_eq!(latest.refresh_token, "rotated-refresh");
        assert!(
            directory
                .path()
                .join("credentials/twitch-client1.json")
                .is_file()
        );
        store.delete("client1").unwrap();
        assert!(store.load("client1").unwrap().is_none());
        store.delete("client1").unwrap();
    }

    #[test]
    fn clients_are_separate_and_identifiers_cannot_escape_directory() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(directory.path().into());
        store.save(&bundle("client1")).unwrap();
        store.save(&bundle("client2")).unwrap();
        store.delete("client1").unwrap();
        assert_eq!(store.load("client2").unwrap().unwrap().client_id, "client2");
        for id in [
            "",
            "../escape",
            "..\\escape",
            "C:\\escape",
            "has space",
            "client.json",
            "nonascii\u{00e9}",
            &"a".repeat(129),
        ] {
            assert!(store.load(id).is_err());
            assert!(store.save(&bundle(id)).is_err());
            assert!(store.delete(id).is_err());
        }
    }

    #[test]
    fn malformed_or_mismatched_credentials_are_not_loaded() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(directory.path().into());
        let path = store.path("client1").unwrap();
        crate::credential_files::write(&path, b"not json").unwrap();
        assert!(store.load("client1").is_err());
        let mut invalid = bundle("client2");
        crate::credential_files::write(&path, &serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(store.load("client1").is_err());
        invalid.client_id = "client1".into();
        invalid.version = 2;
        crate::credential_files::write(&path, &serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(store.load("client1").is_err());
        assert!(store.save(&invalid).is_err());
        invalid.version = 1;
        invalid.refresh_token.clear();
        assert!(store.save(&invalid).is_err());
    }
}
