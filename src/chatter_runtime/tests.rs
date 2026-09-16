use super::*;
use std::path::Path;

fn profile(login: &str, user_id: Option<&str>, never_respond: bool) -> ChatterProfile {
    ChatterProfile {
        id: String::new(),
        user_id: user_id.map(str::to_string),
        login: login.into(),
        nickname: String::new(),
        description: String::new(),
        styles: Styles::default(),
        never_respond,
        revision: 0,
    }
}

fn block_profile_writes(directory: &Path) {
    let chatters = directory.join("chatters");
    if chatters.is_dir() {
        std::fs::remove_dir_all(&chatters).unwrap();
    } else if chatters.exists() {
        std::fs::remove_file(&chatters).unwrap();
    }
    std::fs::write(chatters, b"blocks the required directory").unwrap();
}

#[tokio::test]
async fn pending_profile_binds_then_tracks_rename_without_login_reuse() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = ChatterRuntime::new(directory.path().into(), true);
    runtime.save(profile("viewer", None, false)).await.unwrap();

    runtime.observe("user-1", "viewer", "channel-1");
    let provisional = runtime.admit("user-1", "viewer").unwrap();
    assert_eq!(provisional.profile.as_ref().unwrap().login, "viewer");
    runtime.flush_bindings().await.unwrap();

    let bound = runtime.admit("user-1", "viewer").unwrap();
    assert_eq!(
        bound.profile.as_ref().unwrap().user_id.as_deref(),
        Some("user-1")
    );
    runtime.observe("user-1", "renamed", "channel-1");
    assert!(!bound.valid());
    runtime.flush_bindings().await.unwrap();

    let renamed = runtime.admit("user-1", "renamed").unwrap();
    assert_eq!(renamed.profile.as_ref().unwrap().login, "renamed");
    let reused = runtime.admit("user-2", "viewer").unwrap();
    assert!(reused.profile.is_none());

    let restored = ChatterRuntime::new(directory.path().into(), false);
    restored.load().await;
    assert_eq!(
        restored
            .admit("user-1", "renamed")
            .unwrap()
            .profile
            .as_ref()
            .unwrap()
            .user_id
            .as_deref(),
        Some("user-1")
    );
    assert!(
        restored
            .admit("user-2", "viewer")
            .unwrap()
            .profile
            .is_none()
    );
}

#[tokio::test]
async fn deny_invalidates_only_matching_guard_and_delete_recreate_cannot_revive_it() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = ChatterRuntime::new(directory.path().into(), true);
    runtime
        .save(profile("target", Some("user-1"), false))
        .await
        .unwrap();
    let saved = runtime.view().selected.unwrap();
    let target = runtime.admit("user-1", "target").unwrap();
    let other = runtime.admit("user-2", "other").unwrap();

    let mut denied = saved.clone();
    denied.never_respond = true;
    let affected = runtime.save(denied).await.unwrap();
    assert!(affected.iter().any(|id| id == "user-1"));
    assert!(!target.valid());
    assert!(!runtime.dispatch(&target));
    assert!(other.valid());
    assert!(runtime.dispatch(&other));

    runtime
        .save(profile("cycled", Some("user-3"), false))
        .await
        .unwrap();
    let cycled = runtime.view().selected.unwrap();
    let stale = runtime.admit("user-3", "cycled").unwrap();
    assert!(runtime.dispatch(&stale));
    runtime.delete(&cycled.id).await.unwrap();
    assert!(!stale.valid());
    runtime
        .save(profile("cycled", Some("user-3"), false))
        .await
        .unwrap();
    let replacement = runtime.admit("user-3", "cycled").unwrap();
    assert!(runtime.dispatch(&replacement));
    assert!(!runtime.dispatch(&stale));
}

#[tokio::test]
async fn failed_deny_stays_session_blocked_and_failed_unblock_stays_denied() {
    let failed_first = tempfile::tempdir().unwrap();
    block_profile_writes(failed_first.path());
    let runtime = ChatterRuntime::new(failed_first.path().into(), true);
    assert!(
        runtime
            .save(profile("blocked", Some("user-1"), true))
            .await
            .is_err()
    );
    assert!(!runtime.allowed("user-1", "blocked"));
    assert!(runtime.admit("user-1", "blocked").is_none());
    runtime.query(ChatterFilter::Denied, String::new(), 0, 1);
    let view = runtime.view();
    assert!(view.status.contains("session only"));
    assert!(view.rows.iter().any(|row| row.label.contains("not saved")));

    let failed_unblock = tempfile::tempdir().unwrap();
    let runtime = ChatterRuntime::new(failed_unblock.path().into(), true);
    runtime
        .save(profile("blocked", Some("user-2"), true))
        .await
        .unwrap();
    let mut unblock = runtime.view().selected.unwrap();
    unblock.never_respond = false;
    block_profile_writes(failed_unblock.path());
    assert!(runtime.save(unblock).await.is_err());
    assert!(!runtime.allowed("user-2", "blocked"));
    assert!(runtime.admit("user-2", "blocked").is_none());
}

#[tokio::test]
async fn clearing_seen_preserves_profiles_across_reload() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = ChatterRuntime::new(directory.path().into(), true);
    runtime
        .save(profile("saved", Some("user-1"), false))
        .await
        .unwrap();
    runtime.observe("user-1", "saved", "channel-1");
    runtime.observe("user-2", "seen_only", "channel-1");
    runtime.flush_seen().await.unwrap();
    assert_eq!(runtime.view().seen_count, 2);

    runtime.clear_seen(None).await.unwrap();
    runtime.flush_seen().await.unwrap();
    let current = runtime.view();
    assert_eq!(current.seen_count, 0);
    assert_eq!(current.profile_count, 1);
    assert!(runtime.admit("user-1", "saved").unwrap().profile.is_some());

    let restored = ChatterRuntime::new(directory.path().into(), false);
    restored.load().await;
    let view = restored.view();
    assert_eq!(view.seen_count, 0);
    assert_eq!(view.profile_count, 1);
    assert!(restored.admit("user-1", "saved").unwrap().profile.is_some());
}

#[tokio::test]
async fn corrupt_policy_load_fails_closed_without_destroying_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let chatters = directory.path().join("chatters");
    std::fs::create_dir_all(&chatters).unwrap();
    let policy = chatters.join("chatter-profiles.json");
    let corrupt = br#"{"version":99,"records":[]}"#;
    std::fs::write(&policy, corrupt).unwrap();

    let runtime = ChatterRuntime::new(directory.path().into(), false);
    runtime.load().await;
    let view = runtime.view();
    assert!(!view.ready);
    assert!(view.status.contains("unsupported"));
    assert!(!runtime.allowed("user-1", "viewer"));
    assert!(runtime.admit("user-1", "viewer").is_none());
    assert_eq!(std::fs::read(policy).unwrap(), corrupt);
}

#[tokio::test]
async fn pending_deny_is_union_with_bound_profile_and_conflict_is_visible() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = ChatterRuntime::new(directory.path().into(), true);
    runtime
        .save(profile("old_name", Some("user-1"), false))
        .await
        .unwrap();
    runtime.save(profile("new_name", None, true)).await.unwrap();

    runtime.observe("user-1", "new_name", "channel-1");
    assert!(!runtime.allowed("user-1", "new_name"));
    assert!(runtime.admit("user-1", "new_name").is_none());
    assert!(runtime.view().status.contains("shares a login"));

    runtime.flush_bindings().await.unwrap();
    assert!(!runtime.allowed("user-1", "new_name"));
    assert!(runtime.view().status.contains("shares a login"));
}

#[tokio::test]
async fn queued_pending_binding_is_exclusive_to_its_stable_user_id() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = ChatterRuntime::new(directory.path().into(), true);
    runtime
        .save(profile("reusable_name", None, true))
        .await
        .unwrap();

    runtime.observe("original-user", "reusable_name", "channel-1");
    assert!(!runtime.allowed("original-user", "reusable_name"));

    assert!(runtime.allowed("different-user", "reusable_name"));
    let other = runtime.admit("different-user", "reusable_name").unwrap();
    assert!(other.profile.is_none());
}

#[tokio::test]
async fn retry_can_clear_an_unsaved_deny_and_manual_edit_returns_seen_identity() {
    let directory = tempfile::tempdir().unwrap();
    block_profile_writes(directory.path());
    let runtime = ChatterRuntime::new(directory.path().into(), true);
    assert!(
        runtime
            .save(profile("blocked", Some("user-1"), true))
            .await
            .is_err()
    );
    assert!(!runtime.allowed("user-1", "blocked"));
    std::fs::remove_file(directory.path().join("chatters")).unwrap();
    runtime
        .save(profile("blocked", Some("user-1"), false))
        .await
        .unwrap();
    assert!(runtime.allowed("user-1", "blocked"));
    runtime.observe("user-2", "manual", "channel-1");
    let manual = profile("@MANUAL", None, true);
    assert!(
        runtime
            .affected_users(&manual)
            .contains(&"user-2".to_string())
    );
    let affected = runtime.save(manual).await.unwrap();
    assert!(affected.contains(&"user-2".to_string()));
}
