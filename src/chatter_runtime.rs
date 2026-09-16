//! Bounded chatter policy, observation cache and asynchronous persistence.
use crate::{
    chatter_types::*,
    chatters::{Profiles, SeenStore},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex as AsyncMutex, Notify, watch};

const MAX_SESSION_DENIES: usize = 1_000;
const MAX_UNSAVED_SESSION_DENIES: usize = 500;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct Admission {
    pub user_id: String,
    pub login: String,
    pub profile: Option<ChatterProfile>,
    valid: AtomicBool,
}
impl Admission {
    pub fn valid(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Binding {
    id: String,
    revision: u64,
    user_id: String,
    login: String,
}
struct State {
    profiles: Option<Profiles>,
    seen: Option<SeenStore>,
    bindings: HashMap<String, Binding>,
    session_denies: Vec<ChatterProfile>,
    guards: Vec<Weak<Admission>>,
    seen_generation: u64,
    seen_written: u64,
    policy_error: Option<String>,
    binding_error: Option<String>,
    identity_conflict: Option<String>,
    seen_error: Option<String>,
    view: ChatterView,
    query: (ChatterFilter, String, usize),
}

pub struct ChatterRuntime {
    directory: PathBuf,
    state: Mutex<State>,
    writer: AsyncMutex<()>,
    seen_writer: AsyncMutex<()>,
    pub changed: watch::Sender<u64>,
    pub maintenance: Notify,
}
impl ChatterRuntime {
    pub fn new(directory: PathBuf, ready: bool) -> Arc<Self> {
        let (changed, _) = watch::channel(0);
        Arc::new(Self {
            directory,
            state: Mutex::new(State {
                profiles: ready.then(Profiles::default),
                seen: ready.then(SeenStore::default),
                bindings: HashMap::new(),
                session_denies: Vec::new(),
                guards: Vec::new(),
                seen_generation: 0,
                seen_written: 0,
                policy_error: None,
                binding_error: None,
                identity_conflict: None,
                seen_error: None,
                view: ChatterView::default(),
                query: (ChatterFilter::Saved, String::new(), 0),
            }),
            writer: AsyncMutex::new(()),
            seen_writer: AsyncMutex::new(()),
            changed,
            maintenance: Notify::new(),
        })
    }
    pub async fn load(&self) {
        let directory = self.directory.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            (
                Profiles::load(&directory),
                SeenStore::load(&directory, now()),
            )
        })
        .await;
        let mut state = self.state.lock().unwrap();
        match loaded {
            Ok((profiles, seen)) => {
                match profiles {
                    Ok(p) => state.profiles = Some(p),
                    Err(e) => state.policy_error = Some(e),
                }
                match seen {
                    Ok(s) => {
                        state.seen = Some(s);
                        state.seen_generation += 1;
                    }
                    Err(e) => state.seen_error = Some(e),
                }
            }
            Err(_) => {
                state.policy_error =
                    Some("Could not load chatter profiles. Live replies are suspended.".into())
            }
        }
    }
    fn matches(profile: &ChatterProfile, user_id: &str, login: &str) -> bool {
        profile
            .user_id
            .as_deref()
            .map_or_else(|| profile.login == login, |id| id == user_id)
    }
    fn profile<'a>(state: &'a State, user_id: &str, login: &str) -> Option<&'a ChatterProfile> {
        let store = state.profiles.as_ref()?;
        // A stable ID claimed by a queued binding must outrank another pending
        // profile's login before that binding reaches disk.
        if let Some(profile) = state
            .bindings
            .values()
            .find(|binding| binding.user_id == user_id)
            .and_then(|binding| store.by_id(&binding.id))
        {
            return Some(profile);
        }
        let profile = store.resolve(user_id, login)?;
        if profile.user_id.is_none() && state.bindings.contains_key(&profile.id) {
            return None;
        }
        Some(profile)
    }
    fn pending_deny(state: &State, user_id: &str, login: &str) -> bool {
        state.profiles.as_ref().is_some_and(|profiles| {
            profiles.records().iter().any(|profile| {
                profile.user_id.is_none()
                    && profile.login == login
                    && profile.never_respond
                    && state
                        .bindings
                        .get(&profile.id)
                        .is_none_or(|binding| binding.user_id == user_id)
            })
        })
    }
    fn denied(state: &State, user_id: &str, login: &str) -> bool {
        state.profiles.is_none()
            || state
                .session_denies
                .iter()
                .any(|p| Self::matches(p, user_id, login))
            || Self::profile(state, user_id, login).is_some_and(|p| p.never_respond)
            || Self::pending_deny(state, user_id, login)
    }
    fn invalidate(state: &mut State, profile: &ChatterProfile) {
        state.guards.retain(|weak| {
            if let Some(g) = weak.upgrade() {
                if Self::matches(profile, &g.user_id, &g.login)
                    || g.profile.as_ref().is_some_and(|p| p.id == profile.id)
                {
                    g.valid.store(false, Ordering::Release);
                }
                true
            } else {
                false
            }
        });
    }
    fn same_identity(a: &ChatterProfile, b: &ChatterProfile) -> bool {
        match (a.user_id.as_deref(), b.user_id.as_deref()) {
            (Some(a), Some(b)) => a == b,
            (None, None) => a.login == b.login,
            _ => false,
        }
    }
    fn install_session_deny(state: &mut State, profile: ChatterProfile) -> Result<(), String> {
        state.session_denies.retain(|existing| {
            existing.id != profile.id && !Self::same_identity(existing, &profile)
        });
        let unsaved = !state
            .profiles
            .as_ref()
            .is_some_and(|profiles| profiles.by_id(&profile.id).is_some());
        let unsaved_count = state
            .session_denies
            .iter()
            .filter(|deny| {
                !state
                    .profiles
                    .as_ref()
                    .is_some_and(|profiles| profiles.by_id(&deny.id).is_some())
            })
            .count();
        if state.session_denies.len() >= MAX_SESSION_DENIES
            || (unsaved && unsaved_count >= MAX_UNSAVED_SESSION_DENIES)
        {
            return Err("Too many pending blocks. Retry saving existing blocks first.".into());
        }
        state.session_denies.push(profile);
        Ok(())
    }
    fn refresh_identity_conflict(state: &mut State) {
        let conflict = state.profiles.as_ref().is_some_and(|profiles| {
            profiles
                .records()
                .iter()
                .filter(|profile| profile.user_id.is_none())
                .any(|pending| {
                    profiles.records().iter().any(|bound| {
                        bound.user_id.is_some()
                            && bound.id != pending.id
                            && bound.login == pending.login
                    }) || state.bindings.values().any(|binding| {
                        binding.id != pending.id
                            && binding.login == pending.login
                            && profiles
                                .by_id(&binding.id)
                                .is_some_and(|profile| profile.user_id.is_some())
                    })
                })
        });
        state.identity_conflict = conflict.then(|| {
            "A confirmed chatter now shares a login with a pending profile. Pending never-respond rules remain active until you edit or delete the duplicate.".into()
        });
    }
    fn signal(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub fn observe(&self, user_id: &str, login: &str, channel: &str) {
        let mut state = self.state.lock().unwrap();
        if let Some(seen) = state.seen.as_mut()
            && seen.observe(user_id, login, channel, now())
        {
            state.seen_generation = state.seen_generation.wrapping_add(1);
        }
        let profile = Self::profile(&state, user_id, login).cloned();
        if let Some(p) = profile
            && (p.user_id.is_none() || p.login != login)
            && !state.bindings.contains_key(&p.id)
        {
            let binding = Binding {
                id: p.id.clone(),
                revision: p.revision,
                user_id: user_id.into(),
                login: login.into(),
            };
            state.bindings.insert(p.id.clone(), binding);
            // Once observed, temporary ID aliases prevent a failed write from relaxing a deny.
            if p.never_respond {
                let mut alias = p.clone();
                alias.user_id = Some(user_id.into());
                if let Err(error) = Self::install_session_deny(&mut state, alias) {
                    state.policy_error = Some(error);
                }
            }
            Self::invalidate(&mut state, &p);
            self.signal();
            self.maintenance.notify_one();
        }
        Self::refresh_identity_conflict(&mut state);
    }
    pub fn allowed(&self, user_id: &str, login: &str) -> bool {
        !Self::denied(&self.state.lock().unwrap(), user_id, login)
    }
    pub fn admit(&self, user_id: &str, login: &str) -> Option<Arc<Admission>> {
        let mut state = self.state.lock().unwrap();
        if Self::denied(&state, user_id, login) {
            return None;
        }
        let guard = Arc::new(Admission {
            user_id: user_id.into(),
            login: login.into(),
            profile: Self::profile(&state, user_id, login).cloned(),
            valid: AtomicBool::new(true),
        });
        state.guards.retain(|g| g.strong_count() > 0);
        state.guards.push(Arc::downgrade(&guard));
        Some(guard)
    }
    /// The short state lock is the dispatch gate, shared with deny installation.
    /// Once this returns true the attempt is in flight; network I/O holds no lock.
    pub fn dispatch(&self, guard: &Admission) -> bool {
        let state = self.state.lock().unwrap();
        guard.valid() && !Self::denied(&state, &guard.user_id, &guard.login)
    }
    pub fn view(&self) -> ChatterView {
        let mut state = self.state.lock().unwrap();
        Self::refresh(&mut state);
        state.view.clone()
    }
    fn refresh(state: &mut State) {
        if let Some(selected) = state.view.selected.as_ref().filter(|p| !p.id.is_empty()) {
            let current = state
                .profiles
                .as_ref()
                .and_then(|p| p.by_id(&selected.id))
                .cloned()
                .map(|mut p| {
                    if state.session_denies.iter().any(|s| s.id == p.id) {
                        p.never_respond = true;
                    }
                    p
                });
            if current.as_ref() != Some(selected) {
                state.view.selected = current;
                state.view.selection_serial = state.view.selection_serial.wrapping_add(1);
            }
        }
        state.view.ready = state.profiles.is_some();
        state.view.profile_count = state.profiles.as_ref().map_or(0, |p| p.records().len());
        state.view.seen_count = state.seen.as_ref().map_or(0, |s| s.records().len());
        state.view.estimated_bytes = state.view.seen_count * 512
            + state.profiles.as_ref().map_or(0, |p| {
                p.records()
                    .iter()
                    .map(|r| 768 + r.nickname.len() + r.description.len())
                    .sum::<usize>()
            });
        state.view.status = state.policy_error.clone().or_else(|| state.binding_error.clone()).or_else(|| state.identity_conflict.clone()).or_else(|| state.seen_error.clone()).unwrap_or_else(|| {
            if state.session_denies.is_empty() { "Saved chatter profiles apply across channels on this computer.".into() }
            else { "Some chatters are blocked for this session only. Retry their save to keep this after restart.".into() }
        });
        let (filter, search, offset) = &state.query;
        let mut rows = Vec::new();
        if *filter == ChatterFilter::Seen {
            if let Some(seen) = &state.seen {
                let mut entries: Vec<_> = seen
                    .records()
                    .iter()
                    .filter(|r| r.login.contains(search))
                    .collect();
                entries.sort_by(|a, b| {
                    b.last_seen_at
                        .cmp(&a.last_seen_at)
                        .then(a.login.cmp(&b.login))
                });
                for r in entries {
                    rows.push(ChatterRow {
                        id: format!("seen:{}", r.user_id),
                        label: format!("@{} · seen {}", r.login, timestamp(r.last_seen_at)),
                    });
                }
            }
        } else {
            if let Some(store) = &state.profiles {
                for r in store.records() {
                    let denied =
                        r.never_respond || state.session_denies.iter().any(|s| s.id == r.id);
                    if (*filter != ChatterFilter::Denied || denied)
                        && (r.login.contains(search) || r.nickname.to_lowercase().contains(search))
                    {
                        rows.push(ChatterRow {
                            id: format!("profile:{}", r.id),
                            label: format!(
                                "{}@{}{}{}",
                                if r.nickname.is_empty() {
                                    String::new()
                                } else {
                                    format!("{} · ", r.nickname)
                                },
                                r.login,
                                if denied { " · Never respond" } else { "" },
                                if r.user_id.is_none() {
                                    " · Not seen yet"
                                } else {
                                    ""
                                }
                            ),
                        });
                    }
                }
            }
            // Failed first saves remain visible and retryable rather than becoming hidden denies.
            for r in &state.session_denies {
                if !state
                    .profiles
                    .as_ref()
                    .is_some_and(|p| p.by_id(&r.id).is_some())
                    && r.login.contains(search)
                {
                    rows.push(ChatterRow {
                        id: format!("session:{}", r.id),
                        label: format!("@{} · Blocked this session; not saved", r.login),
                    });
                }
            }
            rows.sort_by(|a, b| a.label.cmp(&b.label));
            rows.dedup_by(|a, b| a.id == b.id);
        }
        state.view.total = rows.len();
        state.view.rows = rows.into_iter().skip(*offset).take(50).collect();
    }
    pub fn query(&self, filter: ChatterFilter, search: String, offset: usize, query_id: u64) {
        let mut state = self.state.lock().unwrap();
        if query_id < state.view.query_id {
            return;
        }
        state.query = (
            filter,
            search
                .chars()
                .take(128)
                .collect::<String>()
                .trim()
                .trim_start_matches('@')
                .to_lowercase(),
            offset.min(2500),
        );
        state.view.query_id = query_id;
    }
    pub fn select(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let selected = if let Some(id) = id.strip_prefix("profile:") {
            state.profiles.as_ref().and_then(|p| p.by_id(id)).cloned()
        } else if let Some(id) = id.strip_prefix("session:") {
            state
                .session_denies
                .iter()
                .find(|p| p.id == id)
                .cloned()
                .map(|mut p| {
                    p.id.clear();
                    p.revision = 0;
                    p
                })
        } else if let Some(id) = id.strip_prefix("seen:") {
            state
                .seen
                .as_ref()
                .and_then(|s| s.records().iter().find(|r| r.user_id == id))
                .map(|r| {
                    Self::profile(&state, &r.user_id, &r.login)
                        .cloned()
                        .unwrap_or_else(|| ChatterProfile {
                            user_id: Some(r.user_id.clone()),
                            login: r.login.clone(),
                            ..Default::default()
                        })
                })
        } else {
            None
        };
        state.view.selected =
            Some(selected.ok_or("Chatter no longer available. Refresh the list.")?);
        state.view.selection_serial = state.view.selection_serial.wrapping_add(1);
        Ok(())
    }
    pub fn ack(&self, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        state.view.action_serial = state.view.action_serial.wrapping_add(1);
        state.view.error = error;
    }
    pub fn reject(&self, error: &str) {
        self.ack(Some(error.into()));
    }

    pub async fn save(&self, profile: ChatterProfile) -> Result<Vec<String>, String> {
        let _writer = self.writer.lock().await;
        let (next, saved, old) = {
            let mut state = self.state.lock().unwrap();
            let mut next = state
                .profiles
                .clone()
                .ok_or("Chatter profiles unavailable. Live replies are suspended.")?;
            let old = next.by_id(&profile.id).cloned();
            let saved = next.save(profile)?;
            if saved.never_respond {
                let mut deny = saved.clone();
                if deny.user_id.is_none()
                    && let Some(binding) = state
                        .bindings
                        .get(&saved.id)
                        .filter(|binding| binding.login == saved.login)
                {
                    deny.user_id = Some(binding.user_id.clone());
                }
                Self::install_session_deny(&mut state, deny)?;
                Self::invalidate(&mut state, &saved);
                self.signal();
            }
            (next, saved, old)
        };
        let directory = self.directory.clone();
        let next = tokio::task::spawn_blocking(move || {
            next.persist(&directory)?;
            Ok::<_, String>(next)
        })
        .await
        .map_err(|_| "Chatter writer failed.")??;
        let mut state = self.state.lock().unwrap();
        if let Some(old) = &old {
            Self::invalidate(&mut state, old);
        }
        Self::invalidate(&mut state, &saved);
        let mut users = Self::known_users(&state, &saved);
        if let Some(old) = &old {
            users.extend(Self::known_users(&state, old));
        }
        if let Some(id) = &saved.user_id {
            users.push(id.clone());
        }
        for weak in &state.guards {
            if let Some(g) = weak.upgrade()
                && !g.valid()
            {
                users.push(g.user_id.clone());
            }
        }
        let mut keep_binding_alias = None;
        let mut discard_binding = false;
        if let Some(binding) = state.bindings.get_mut(&saved.id) {
            let compatible = saved
                .user_id
                .as_deref()
                .map_or(binding.login == saved.login, |id| id == binding.user_id);
            users.push(binding.user_id.clone());
            if compatible {
                binding.revision = saved.revision;
                if saved.user_id.is_none() && saved.never_respond {
                    keep_binding_alias = Some(binding.user_id.clone());
                }
            } else {
                discard_binding = true;
            }
        }
        if discard_binding {
            state.bindings.remove(&saved.id);
        }
        state
            .session_denies
            .retain(|profile| profile.id != saved.id && !Self::same_identity(profile, &saved));
        state.profiles = Some(next);
        if let Some(user_id) = keep_binding_alias {
            let mut alias = saved.clone();
            alias.user_id = Some(user_id);
            // Replacing the same profile cannot exceed the bounded deny set.
            let _ = Self::install_session_deny(&mut state, alias);
            self.maintenance.notify_one();
        }
        if state.bindings.is_empty() {
            state.binding_error = None;
        }
        Self::refresh_identity_conflict(&mut state);
        state.view.selected = Some(saved);
        state.view.selection_serial = state.view.selection_serial.wrapping_add(1);
        self.signal();
        Ok(users)
    }
    pub async fn delete(&self, id: &str) -> Result<Vec<String>, String> {
        let _writer = self.writer.lock().await;
        let (mut next, old) = {
            let state = self.state.lock().unwrap();
            let store = state
                .profiles
                .clone()
                .ok_or("Chatter profiles unavailable.")?;
            let old = store
                .by_id(id)
                .cloned()
                .ok_or("Chatter profile not found.")?;
            (store, old)
        };
        next.delete(id)?;
        let directory = self.directory.clone();
        let next = tokio::task::spawn_blocking(move || {
            next.persist(&directory)?;
            Ok::<_, String>(next)
        })
        .await
        .map_err(|_| "Chatter writer failed.")??;
        let mut state = self.state.lock().unwrap();
        Self::invalidate(&mut state, &old);
        let mut users = Self::known_users(&state, &old);
        if let Some(b) = state.bindings.remove(id) {
            users.push(b.user_id);
        }
        if state.bindings.is_empty() {
            state.binding_error = None;
        }
        state.session_denies.retain(|p| p.id != id);
        state.profiles = Some(next);
        Self::refresh_identity_conflict(&mut state);
        state.view.selected = None;
        state.view.selection_serial = state.view.selection_serial.wrapping_add(1);
        self.signal();
        Ok(users)
    }
    pub async fn flush_bindings(&self) -> Result<(), String> {
        let _writer = self.writer.lock().await;
        let (mut next, bindings) = {
            let state = self.state.lock().unwrap();
            if state.bindings.is_empty() {
                return Ok(());
            }
            (
                state
                    .profiles
                    .clone()
                    .ok_or("Chatter profiles unavailable.")?,
                state.bindings.values().cloned().collect::<Vec<_>>(),
            )
        };
        let mut applied = Vec::new();
        let mut skipped = Vec::new();
        for binding in &bindings {
            let Some(current) = next.by_id(&binding.id).cloned() else {
                skipped.push(binding.clone());
                continue;
            };
            let compatible = current
                .user_id
                .as_deref()
                .map_or(current.login == binding.login, |id| id == binding.user_id);
            if !compatible {
                skipped.push(binding.clone());
                continue;
            }
            match next.bind(
                &binding.id,
                current.revision,
                &binding.user_id,
                &binding.login,
            ) {
                Ok(_) => applied.push(binding.clone()),
                Err(_) => skipped.push(binding.clone()),
            }
        }
        if applied.is_empty() {
            let mut state = self.state.lock().unwrap();
            for binding in &skipped {
                if state.bindings.get(&binding.id) == Some(binding) {
                    state.bindings.remove(&binding.id);
                }
            }
            state.binding_error = (!skipped.is_empty()).then(|| {
                "Some chatter identities changed before they could be saved. Their session blocks remain active.".into()
            });
            Self::refresh_identity_conflict(&mut state);
            self.signal();
            return Ok(());
        }
        let directory = self.directory.clone();
        let next = tokio::task::spawn_blocking(move || {
            next.persist(&directory)?;
            Ok::<_, String>(next)
        })
        .await
        .map_err(|_| "Chatter identity writer failed.")??;
        let mut state = self.state.lock().unwrap();
        for b in &applied {
            if let Some(old) = state
                .profiles
                .as_ref()
                .and_then(|p| p.by_id(&b.id))
                .cloned()
            {
                Self::invalidate(&mut state, &old);
            }
            if state.bindings.get(&b.id) == Some(b) {
                state.bindings.remove(&b.id);
            }
            state
                .session_denies
                .retain(|p| p.id != b.id || !next.by_id(&b.id).is_some_and(|n| n.never_respond));
        }
        for b in &skipped {
            if state.bindings.get(&b.id) == Some(b) {
                state.bindings.remove(&b.id);
            }
        }
        state.binding_error = (!skipped.is_empty()).then(|| {
            "Some chatter identities changed before they could be saved. Their session blocks remain active.".into()
        });
        state.profiles = Some(next);
        Self::refresh_identity_conflict(&mut state);
        self.signal();
        Ok(())
    }
    pub async fn flush_seen(&self) -> Result<(), String> {
        let _writer = self.seen_writer.lock().await;
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            let pruned = state.seen.as_mut().is_some_and(|seen| seen.prune(now()));
            if pruned {
                state.seen_generation = state.seen_generation.wrapping_add(1);
            }
            if state.seen_generation == state.seen_written {
                return Ok(());
            }
            state.seen.clone().map(|s| (s, state.seen_generation))
        };
        let Some((seen, generation)) = snapshot else {
            return Ok(());
        };
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || seen.persist(&directory))
            .await
            .map_err(|_| "Seen chatter writer failed.")??;
        let mut state = self.state.lock().unwrap();
        state.seen_written = generation;
        state.seen_error = None;
        Ok(())
    }
    pub async fn clear_seen(&self, user_id: Option<String>) -> Result<(), String> {
        let _writer = self.seen_writer.lock().await;
        let (mut next, generation) = {
            let s = self.state.lock().unwrap();
            (s.seen.clone().unwrap_or_default(), s.seen_generation)
        };
        if let Some(id) = &user_id {
            next.forget(id);
        } else {
            next.clear();
        }
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || next.persist(&directory))
            .await
            .map_err(|_| "Seen chatter writer failed.")??;
        let mut state = self.state.lock().unwrap();
        // Apply the same deletion to the current cache; keep observations made during I/O dirty.
        let seen = state.seen.get_or_insert_with(SeenStore::default);
        if let Some(id) = &user_id {
            seen.forget(id);
        } else {
            seen.clear();
        }
        state.seen_written = generation;
        state.seen_generation = state.seen_generation.wrapping_add(1);
        state.seen_error = None;
        Ok(())
    }
    pub fn binding_error(&self, error: String) {
        self.state.lock().unwrap().binding_error = Some(error);
    }
    pub fn persistence_error(&self, error: String) {
        self.state.lock().unwrap().seen_error = Some(error);
    }
    pub fn affected_users(&self, profile: &ChatterProfile) -> Vec<String> {
        let state = self.state.lock().unwrap();
        let mut normalized = profile.clone();
        normalized.login = normalized
            .login
            .trim()
            .strip_prefix('@')
            .unwrap_or(normalized.login.trim())
            .to_ascii_lowercase();
        Self::known_users(&state, &normalized)
    }
    fn known_users(state: &State, profile: &ChatterProfile) -> Vec<String> {
        let mut users: Vec<_> = profile.user_id.iter().cloned().collect();
        if let Some(seen) = &state.seen {
            for r in seen.records() {
                if Self::matches(profile, &r.user_id, &r.login) {
                    users.push(r.user_id.clone());
                }
            }
        }
        users
    }
}
fn timestamp(seconds: u64) -> String {
    let elapsed = now().saturating_sub(seconds);
    if elapsed < 60 {
        "just now".into()
    } else if elapsed < 3600 {
        format!("{}m ago", elapsed / 60)
    } else if elapsed < 86400 {
        format!("{}h ago", elapsed / 3600)
    } else {
        format!("{}d ago", elapsed / 86400)
    }
}

#[cfg(test)]
mod tests;
