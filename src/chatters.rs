//! Bounded, private persistence for curated chatter profiles and recently seen chatters.
//! Chat text is never stored here.
use crate::{
    chatter_prompt,
    chatter_types::{ChatterProfile, SeenChatter},
    config::{Config, valid_login},
    credential_files,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

const VERSION: u32 = 1;
const MAX_PROFILES: usize = 500;
const MAX_PROFILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SEEN: usize = 2_000;
const MAX_SEEN_BYTES: u64 = 1024 * 1024;
const SEEN_RETENTION_SECONDS: u64 = 90 * 24 * 60 * 60;
const PROFILE_FILE: &str = "chatter-profiles.json";
const SEEN_FILE: &str = "seen-chatters.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    version: u32,
    records: Vec<ChatterProfile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeenFile {
    version: u32,
    records: Vec<SeenChatter>,
}

#[derive(Clone, Default)]
pub struct Profiles {
    records: Vec<ChatterProfile>,
    by_id: HashMap<String, usize>,
    bound_by_user: HashMap<String, usize>,
    pending_by_login: HashMap<String, usize>,
}

impl Profiles {
    pub fn load(directory: &Path) -> Result<Self, String> {
        let path = profile_path(directory);
        let Some(bytes) = credential_files::read_bounded(&path, MAX_PROFILE_BYTES).map_err(
            |_| "Could not read saved chatter profiles. Check the data-folder permissions.",
        )?
        else {
            return Ok(Self::default());
        };
        let file: ProfileFile = serde_json::from_slice(&bytes).map_err(|_| {
            "The saved chatter profile file is invalid. Restore or remove it before using chatter profiles."
        })?;
        if file.version != VERSION {
            return Err("The saved chatter profile file uses an unsupported version.".into());
        }
        let mut profiles = Self {
            records: file.records,
            ..Self::default()
        };
        profiles.rebuild_indexes()?;
        profiles.validate()?;
        Ok(profiles)
    }

    pub fn persist(&self, directory: &Path) -> Result<(), String> {
        self.validate()?;
        let bytes = self.encoded()?;
        credential_files::write_bounded(&profile_path(directory), &bytes, MAX_PROFILE_BYTES)
            .map_err(|_| {
                "Could not save chatter profiles. Check the data-folder permissions.".into()
            })
    }

    pub fn records(&self) -> &[ChatterProfile] {
        &self.records
    }

    /// Resolve a live chatter by stable Twitch ID first. Login fallback is only for
    /// manually entered profiles that have not been bound to a Twitch ID.
    pub fn resolve(&self, user_id: &str, login: &str) -> Option<&ChatterProfile> {
        self.bound_by_user
            .get(user_id)
            .or_else(|| self.pending_by_login.get(login))
            .and_then(|index| self.records.get(*index))
    }

    pub fn by_id(&self, id: &str) -> Option<&ChatterProfile> {
        self.by_id
            .get(id)
            .and_then(|index| self.records.get(*index))
    }

    pub fn save(&mut self, mut profile: ChatterProfile) -> Result<ChatterProfile, String> {
        normalize_profile(&mut profile);
        let mut next = self.clone();
        if profile.id.is_empty() {
            if next.records.len() >= MAX_PROFILES {
                return Err("You can save up to 500 chatter profiles.".into());
            }
            profile.id = format!("c-{:032x}", fastrand::u128(..));
            profile.revision = 1;
            validate_profile(&profile)?;
            next.reject_identity_conflict(&profile, None)?;
            next.records.push(profile.clone());
        } else {
            validate_profile_id(&profile.id)?;
            let index = *next
                .by_id
                .get(&profile.id)
                .ok_or("That chatter profile no longer exists. Refresh and try again.")?;
            let current = &next.records[index];
            if profile.revision != current.revision {
                return Err("That chatter profile changed. Refresh and try again.".into());
            }
            if profile.user_id != current.user_id
                || (current.user_id.is_some() && profile.login != current.login)
            {
                return Err("A confirmed Twitch identity cannot be edited manually.".into());
            }
            profile.revision = current
                .revision
                .checked_add(1)
                .ok_or("That chatter profile can no longer be revised.")?;
            validate_profile(&profile)?;
            next.reject_identity_conflict(&profile, Some(index))?;
            next.records[index] = profile.clone();
        }
        next.rebuild_indexes()?;
        next.validate()?;
        *self = next;
        Ok(profile)
    }

    pub fn delete(&mut self, id: &str) -> Result<ChatterProfile, String> {
        let index = *self
            .by_id
            .get(id)
            .ok_or("That chatter profile no longer exists.")?;
        let removed = self.records.remove(index);
        self.rebuild_indexes()?;
        Ok(removed)
    }

    /// Bind a pending profile, or refresh the login for the same stable Twitch ID.
    /// Callers persist a cloned store before installing it as applied state.
    pub fn bind(
        &mut self,
        id: &str,
        expected_revision: u64,
        user_id: &str,
        login: &str,
    ) -> Result<bool, String> {
        validate_user_id(user_id)?;
        if !valid_login(login) {
            return Err("Invalid Twitch login for chatter identity.".into());
        }
        let index = *self
            .by_id
            .get(id)
            .ok_or("That chatter profile no longer exists.")?;
        let current = &self.records[index];
        if current.revision != expected_revision {
            return Err("That chatter profile changed. Refresh and try again.".into());
        }
        match current.user_id.as_deref() {
            None if current.login != login => {
                return Err("The observed Twitch login does not match the pending profile.".into());
            }
            Some(bound) if bound != user_id => {
                return Err("A confirmed Twitch identity cannot be rebound.".into());
            }
            _ => {}
        }
        if self
            .bound_by_user
            .get(user_id)
            .is_some_and(|other| *other != index)
        {
            return Err("That Twitch account already has a saved chatter profile.".into());
        }
        if current.user_id.as_deref() == Some(user_id) && current.login == login {
            return Ok(false);
        }
        let revision = current
            .revision
            .checked_add(1)
            .ok_or("That chatter profile can no longer be revised.")?;
        self.records[index].user_id = Some(user_id.into());
        self.records[index].login = login.into();
        self.records[index].revision = revision;
        self.rebuild_indexes()?;
        self.validate()?;
        Ok(true)
    }

    fn reject_identity_conflict(
        &self,
        profile: &ChatterProfile,
        replacing: Option<usize>,
    ) -> Result<(), String> {
        for (index, existing) in self.records.iter().enumerate() {
            if Some(index) == replacing {
                continue;
            }
            if profile
                .user_id
                .as_ref()
                .zip(existing.user_id.as_ref())
                .is_some_and(|(a, b)| a == b)
            {
                return Err("That Twitch account already has a saved chatter profile.".into());
            }
            if existing.login == profile.login
                && (existing.user_id.is_none() || profile.user_id.is_none())
            {
                return Err("That Twitch login already has a saved chatter profile.".into());
            }
        }
        Ok(())
    }

    fn rebuild_indexes(&mut self) -> Result<(), String> {
        let mut by_id = HashMap::with_capacity(self.records.len());
        let mut bound_by_user = HashMap::with_capacity(self.records.len());
        let mut pending_by_login = HashMap::with_capacity(self.records.len());
        for (index, profile) in self.records.iter().enumerate() {
            if by_id.insert(profile.id.clone(), index).is_some() {
                return Err(
                    "The saved chatter profile file contains duplicate identifiers.".into(),
                );
            }
            if let Some(user_id) = &profile.user_id {
                if bound_by_user.insert(user_id.clone(), index).is_some() {
                    return Err(
                        "The saved chatter profile file contains duplicate Twitch accounts.".into(),
                    );
                }
            } else if pending_by_login
                .insert(profile.login.clone(), index)
                .is_some()
            {
                return Err(
                    "The saved chatter profile file contains duplicate pending logins.".into(),
                );
            }
        }
        self.by_id = by_id;
        self.bound_by_user = bound_by_user;
        self.pending_by_login = pending_by_login;
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        if self.records.len() > MAX_PROFILES {
            return Err("The saved chatter profile file contains too many profiles.".into());
        }
        for profile in &self.records {
            validate_profile(profile)?;
        }
        if self.encoded()?.len() as u64 > MAX_PROFILE_BYTES {
            return Err("Saved chatter profiles exceed the 2 MiB storage limit.".into());
        }
        Ok(())
    }

    fn encoded(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&ProfileFile {
            version: VERSION,
            records: self.records.clone(),
        })
        .map_err(|_| "Could not encode chatter profiles.".into())
    }
}

#[derive(Clone, Default)]
pub struct SeenStore {
    records: Vec<SeenChatter>,
    by_user: HashMap<String, usize>,
}

impl SeenStore {
    pub fn load(directory: &Path, now: u64) -> Result<Self, String> {
        let path = seen_path(directory);
        let Some(bytes) = credential_files::read_bounded(&path, MAX_SEEN_BYTES)
            .map_err(|_| "Could not read seen chatters. Check the data-folder permissions.")?
        else {
            return Ok(Self::default());
        };
        let file: SeenFile = serde_json::from_slice(&bytes).map_err(
            |_| "The seen chatter file is invalid. Clear it before collecting new chatters.",
        )?;
        if file.version != VERSION {
            return Err("The seen chatter file uses an unsupported version.".into());
        }
        let mut store = Self {
            records: file.records,
            ..Self::default()
        };
        store.rebuild_index()?;
        store.validate_records()?;
        store.prune(now);
        store.validate()?;
        Ok(store)
    }

    pub fn persist(&self, directory: &Path) -> Result<(), String> {
        self.validate()?;
        let bytes = self.encoded()?;
        if bytes.len() as u64 > MAX_SEEN_BYTES {
            return Err("Seen chatters exceed the 1 MiB storage limit.".into());
        }
        credential_files::write_bounded(&seen_path(directory), &bytes, MAX_SEEN_BYTES)
            .map_err(|_| "Could not save seen chatters. Check the data-folder permissions.".into())
    }

    pub fn records(&self) -> &[SeenChatter] {
        &self.records
    }

    /// Returns true when persistent state changed. Timestamp-only changes are
    /// coarsened to one update per wall-clock minute for each chatter.
    pub fn observe(&mut self, user_id: &str, login: &str, channel: &str, now: u64) -> bool {
        if validate_seen_identity(user_id, login, channel).is_err() {
            return false;
        }
        if let Some(index) = self.by_user.get(user_id).copied() {
            let record = &mut self.records[index];
            let metadata_changed = record.login != login || record.last_seen_channel_id != channel;
            let minute_changed = now > record.last_seen_at && now / 60 != record.last_seen_at / 60;
            if metadata_changed || minute_changed {
                record.login = login.into();
                record.last_seen_channel_id = channel.into();
                record.last_seen_at = record.last_seen_at.max(now);
                return true;
            }
            return false;
        }

        if self.records.len() >= MAX_SEEN {
            let oldest = self
                .records
                .iter()
                .enumerate()
                .min_by_key(|(_, record)| record.last_seen_at)
                .map(|(index, _)| index)
                .unwrap_or(0);
            self.records.remove(oldest);
            let _ = self.rebuild_index();
        }
        let index = self.records.len();
        self.records.push(SeenChatter {
            user_id: user_id.into(),
            login: login.into(),
            last_seen_at: now,
            last_seen_channel_id: channel.into(),
        });
        self.by_user.insert(user_id.into(), index);
        true
    }

    pub fn forget(&mut self, user_id: &str) -> bool {
        let Some(index) = self.by_user.get(user_id).copied() else {
            return false;
        };
        self.records.remove(index);
        let _ = self.rebuild_index();
        true
    }

    pub fn clear(&mut self) -> bool {
        if self.records.is_empty() {
            return false;
        }
        self.records.clear();
        self.by_user.clear();
        true
    }

    fn rebuild_index(&mut self) -> Result<(), String> {
        let mut by_user = HashMap::with_capacity(self.records.len());
        for (index, record) in self.records.iter().enumerate() {
            if by_user.insert(record.user_id.clone(), index).is_some() {
                return Err("The seen chatter file contains duplicate Twitch accounts.".into());
            }
        }
        self.by_user = by_user;
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        if self.records.len() > MAX_SEEN {
            return Err("The seen chatter file contains too many records.".into());
        }
        self.validate_records()
    }

    fn validate_records(&self) -> Result<(), String> {
        let mut users = HashSet::with_capacity(self.records.len());
        for record in &self.records {
            validate_seen_identity(&record.user_id, &record.login, &record.last_seen_channel_id)?;
            if !users.insert(&record.user_id) {
                return Err("The seen chatter file contains duplicate Twitch accounts.".into());
            }
        }
        Ok(())
    }

    /// Apply age and encoded-size retention outside the per-message hot path.
    pub fn prune(&mut self, now: u64) -> bool {
        let before = self.records.len();
        self.records
            .retain(|record| now.saturating_sub(record.last_seen_at) <= SEEN_RETENTION_SECONDS);
        if self.records.len() > MAX_SEEN {
            self.records.sort_by_key(|record| record.last_seen_at);
            self.records.drain(..self.records.len() - MAX_SEEN);
        }
        while self
            .encoded()
            .is_ok_and(|bytes| bytes.len() as u64 > MAX_SEEN_BYTES)
            && !self.records.is_empty()
        {
            let oldest = self
                .records
                .iter()
                .enumerate()
                .min_by_key(|(_, record)| record.last_seen_at)
                .map(|(index, _)| index)
                .unwrap_or(0);
            self.records.remove(oldest);
        }
        let changed = before != self.records.len();
        if changed {
            let _ = self.rebuild_index();
        }
        changed
    }

    fn encoded(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&SeenFile {
            version: VERSION,
            records: self.records.clone(),
        })
        .map_err(|_| "Could not encode seen chatters.".into())
    }
}

fn profile_path(directory: &Path) -> PathBuf {
    directory.join("chatters").join(PROFILE_FILE)
}

fn seen_path(directory: &Path) -> PathBuf {
    directory.join("chatters").join(SEEN_FILE)
}

fn normalize_profile(profile: &mut ChatterProfile) {
    profile.login = profile
        .login
        .trim()
        .strip_prefix('@')
        .unwrap_or(profile.login.trim())
        .to_ascii_lowercase();
    profile.nickname = profile.nickname.trim().into();
    profile.description = profile.description.trim().into();
}

fn validate_profile(profile: &ChatterProfile) -> Result<(), String> {
    validate_profile_id(&profile.id)?;
    if !valid_login(&profile.login) {
        return Err(
            "Enter a Twitch login using lowercase letters, digits, and underscores.".into(),
        );
    }
    if let Some(user_id) = &profile.user_id {
        validate_user_id(user_id)?;
    }
    if profile.nickname.chars().count() > 64
        || profile.nickname.len() > 256
        || profile.nickname.chars().any(char::is_control)
    {
        return Err(
            "Nickname must be at most 64 characters and 256 bytes without control characters."
                .into(),
        );
    }
    if profile.description.chars().count() > 500
        || profile.description.len() > 2_000
        || profile
            .description
            .chars()
            .any(|character| character.is_control() && character != '\n')
    {
        return Err("Description must be at most 500 characters and 2,000 bytes; only newlines may be control characters.".into());
    }
    if profile.revision == 0 {
        return Err("Invalid chatter profile revision.".into());
    }
    let mut prompt_profile = profile.clone();
    prompt_profile.never_respond = false;
    chatter_prompt::system_prompt(&Config::default(), Some(&prompt_profile))?;
    Ok(())
}

fn validate_profile_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 40
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("Invalid chatter profile identifier.".into());
    }
    Ok(())
}

fn validate_user_id(user_id: &str) -> Result<(), String> {
    if user_id.is_empty()
        || user_id.len() > 128
        || !user_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("Invalid Twitch user identifier.".into());
    }
    Ok(())
}

fn validate_seen_identity(user_id: &str, login: &str, channel: &str) -> Result<(), String> {
    validate_user_id(user_id)?;
    if !valid_login(login) {
        return Err("Invalid Twitch login in seen chatters.".into());
    }
    validate_user_id(channel).map_err(|_| "Invalid Twitch channel identifier.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatter_types::Styles;

    fn draft(login: &str) -> ChatterProfile {
        ChatterProfile {
            id: String::new(),
            user_id: None,
            login: login.into(),
            nickname: String::new(),
            description: String::new(),
            styles: Styles::default(),
            never_respond: false,
            revision: 0,
        }
    }

    #[test]
    fn revisions_are_compare_and_swap_and_bound_identity_is_not_editable() {
        let mut profiles = Profiles::default();
        let saved = profiles.save(draft("viewer")).unwrap();
        assert_eq!(saved.revision, 1);
        let mut edit = saved.clone();
        edit.nickname = "Friend".into();
        let edited = profiles.save(edit.clone()).unwrap();
        assert_eq!(edited.revision, 2);
        assert!(profiles.save(edit).is_err());

        assert!(
            profiles
                .bind(&edited.id, edited.revision, "user-1", "viewer")
                .unwrap()
        );
        let bound = profiles.by_id(&edited.id).unwrap().clone();
        assert_eq!(bound.revision, 3);
        let mut forged = bound.clone();
        forged.login = "someone_else".into();
        assert!(profiles.save(forged).is_err());
    }

    #[test]
    fn stable_id_wins_and_pending_login_never_matches_a_bound_old_login() {
        let mut profiles = Profiles::default();
        let pending = profiles.save(draft("pending")).unwrap();
        assert_eq!(
            profiles.resolve("unknown", "pending").unwrap().id,
            pending.id
        );
        profiles
            .bind(&pending.id, pending.revision, "user-1", "pending")
            .unwrap();
        let revision = profiles.by_id(&pending.id).unwrap().revision;
        profiles
            .bind(&pending.id, revision, "user-1", "renamed")
            .unwrap();
        assert!(profiles.resolve("other-user", "pending").is_none());
        assert_eq!(
            profiles.resolve("user-1", "anything").unwrap().id,
            pending.id
        );
    }

    #[test]
    fn profiles_round_trip_and_invalid_file_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let mut profiles = Profiles::default();
        let saved = profiles.save(draft("viewer")).unwrap();
        profiles.persist(directory.path()).unwrap();
        let restored = Profiles::load(directory.path()).unwrap();
        assert_eq!(restored.by_id(&saved.id).unwrap().login, "viewer");

        let path = profile_path(directory.path());
        let invalid = br#"{"version":99,"records":[]}"#;
        credential_files::write_bounded(&path, invalid, MAX_PROFILE_BYTES).unwrap();
        assert!(Profiles::load(directory.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), invalid);
    }

    #[test]
    fn profile_text_limits_count_scalars_and_bytes() {
        let mut profiles = Profiles::default();
        let mut too_many_scalars = draft("viewer");
        too_many_scalars.nickname = "a".repeat(65);
        assert!(profiles.save(too_many_scalars).is_err());
        let mut too_many_bytes = draft("viewer");
        too_many_bytes.nickname = "😀".repeat(64);
        assert!(profiles.save(too_many_bytes).is_ok());
        let mut description = draft("second");
        description.description = "line one\nline two".into();
        assert!(profiles.save(description).is_ok());
        let mut controlled = draft("third");
        controlled.description = "bad\ttext".into();
        assert!(profiles.save(controlled).is_err());

        let normalized = profiles.save(draft("  @Mixed_Case  ")).unwrap();
        assert_eq!(normalized.login, "mixed_case");
    }

    #[test]
    fn seen_updates_once_per_minute_and_tracks_renames_immediately() {
        let mut seen = SeenStore::default();
        assert!(seen.observe("user-1", "viewer", "channel-1", 120));
        assert!(!seen.observe("user-1", "viewer", "channel-1", 121));
        assert!(seen.observe("user-1", "renamed", "channel-1", 122));
        assert_eq!(seen.records()[0].last_seen_at, 122);
        assert!(seen.observe("user-1", "renamed", "channel-1", 180));
    }

    #[test]
    fn seen_is_bounded_and_expired_records_are_pruned() {
        let mut seen = SeenStore::default();
        for index in 0..=MAX_SEEN {
            assert!(seen.observe(
                &format!("user-{index}"),
                &format!("viewer_{index}"),
                "channel-1",
                index as u64 + 1,
            ));
        }
        assert_eq!(seen.records().len(), MAX_SEEN);
        assert!(
            !seen
                .records()
                .iter()
                .any(|record| record.user_id == "user-0")
        );
        assert!(seen.observe(
            "fresh-user",
            "fresh_viewer",
            "channel-1",
            SEEN_RETENTION_SECONDS + 10,
        ));
        assert!(seen.prune(SEEN_RETENTION_SECONDS + 10));
        assert!(seen.records().len() < MAX_SEEN);
        assert!(
            seen.records()
                .iter()
                .any(|record| record.user_id == "fresh-user")
        );
    }

    #[test]
    fn seen_round_trips_and_forget_is_separate_from_profiles() {
        let directory = tempfile::tempdir().unwrap();
        let mut seen = SeenStore::default();
        seen.observe("user-1", "viewer", "channel-1", 100);
        seen.persist(directory.path()).unwrap();
        let mut restored = SeenStore::load(directory.path(), 101).unwrap();
        assert_eq!(restored.records().len(), 1);
        assert!(restored.forget("user-1"));
        assert!(!restored.forget("user-1"));
        assert!(!restored.clear());
    }
}
