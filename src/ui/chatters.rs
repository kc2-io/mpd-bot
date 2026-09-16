//! Native chatter-directory editor callbacks and snapshot rendering.
//!
//! The application owns persistence and query ordering. This module only keeps
//! a local draft in the Slint window and converts the selected, immutable
//! identity from the latest snapshot into a save action.
use crate::{
    application::{AppCommand, AppSnapshot, DesktopHandle},
    chatter_types::{ChatterAction, ChatterFilter, ChatterProfile, ChatterView, Styles},
};
use mpd_bot_view::DesktopWindow;
use slint::ComponentHandle;

fn submit(window: &DesktopWindow, handle: &DesktopHandle, command: ChatterAction) -> bool {
    match handle.commands.try_send(AppCommand::Chatter(command)) {
        Ok(()) => true,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            window.set_notice("The app is busy applying another action. Try again shortly.".into());
            false
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            window.set_notice("The app runtime has stopped. Quit and restart MPD Bot.".into());
            false
        }
    }
}

fn filter(index: i32) -> ChatterFilter {
    match index {
        1 => ChatterFilter::Seen,
        2 => ChatterFilter::Denied,
        _ => ChatterFilter::Saved,
    }
}

fn request_query(window: &DesktopWindow, handle: &DesktopHandle, offset: i32) {
    let query_id = window
        .get_chatter_requested_query_id()
        .wrapping_add(1)
        .max(1);
    let offset = offset.max(0);
    if submit(
        window,
        handle,
        ChatterAction::Query {
            filter: filter(window.get_chatter_filter()),
            search: window.get_chatter_search().trim().to_string(),
            offset: offset as usize,
            query_id: query_id as u64,
        },
    ) {
        window.set_chatter_requested_query_id(query_id);
        window.set_chatter_offset(offset);
    }
}

fn apply_selected(window: &DesktopWindow, profile: Option<&ChatterProfile>) {
    if let Some(profile) = profile {
        window.set_chatter_id(profile.id.clone().into());
        window.set_chatter_revision(profile.revision.to_string().into());
        window.set_chatter_user_id(profile.user_id.clone().unwrap_or_default().into());
        window.set_chatter_login(profile.login.clone().into());
        window.set_chatter_nickname(profile.nickname.clone().into());
        window.set_chatter_description(profile.description.clone().into());
        window.set_chatter_sarcastic(profile.styles.sarcastic);
        window.set_chatter_praise(profile.styles.praise);
        window.set_chatter_hero(profile.styles.hero);
        window.set_chatter_regular(profile.styles.regular);
        window.set_chatter_never_respond(profile.never_respond);
    } else {
        window.invoke_reset_chatter_draft();
    }
}

/// Build a save payload from the window's editable fields while taking the
/// identity and optimistic revision only from the latest app snapshot.
fn save_profile(window: &DesktopWindow, snapshot: &AppSnapshot) -> Option<ChatterProfile> {
    let selected = snapshot.chatters.selected.as_ref();
    let draft_id = window.get_chatter_id().to_string();
    let draft_user_id = window.get_chatter_user_id().to_string();

    let (id, user_id, login, revision) = match selected {
        Some(profile)
            if (!draft_id.is_empty() && profile.id == draft_id)
                || (draft_id.is_empty()
                    && !draft_user_id.is_empty()
                    && profile.id.is_empty()
                    && profile.user_id.as_deref() == Some(draft_user_id.as_str())) =>
        {
            (
                profile.id.clone(),
                profile.user_id.clone(),
                if profile.user_id.is_some() {
                    profile.login.clone()
                } else {
                    window.get_chatter_login().trim().to_string()
                },
                window.get_chatter_revision().parse().unwrap_or(0),
            )
        }
        // A manual new draft deliberately has no bound identity yet.
        _ if draft_id.is_empty() && draft_user_id.is_empty() => (
            String::new(),
            None,
            window.get_chatter_login().trim().to_string(),
            0,
        ),
        _ => return None,
    };

    Some(ChatterProfile {
        id,
        user_id,
        login,
        nickname: window.get_chatter_nickname().trim().to_string(),
        description: window.get_chatter_description().trim().to_string(),
        styles: Styles {
            sarcastic: window.get_chatter_sarcastic(),
            praise: window.get_chatter_praise(),
            hero: window.get_chatter_hero(),
            regular: window.get_chatter_regular(),
        },
        never_respond: window.get_chatter_never_respond(),
        revision,
    })
}

pub(super) fn install(window: &DesktopWindow, handle: &DesktopHandle) {
    window.on_chatter_character_count(|s| s.chars().count().min(i32::MAX as usize) as i32);
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        let timer = std::rc::Rc::new(slint::Timer::default());
        window.on_query_chatters(move |offset| {
            if let Some(window) = weak.upgrade()
                && !window.get_chatter_busy()
                && !window.get_chatter_dirty()
            {
                let weak = window.as_weak();
                let handle = handle.clone();
                timer.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_millis(150),
                    move || {
                        if let Some(window) = weak.upgrade()
                            && !window.get_chatter_dirty()
                            && !window.get_chatter_busy()
                        {
                            request_query(&window, &handle, offset);
                        }
                    },
                );
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        window.on_select_chatter(move |id| {
            if let Some(window) = weak.upgrade()
                && !window.get_chatter_busy()
                && !window.get_chatter_dirty()
            {
                let _ = submit(&window, &handle, ChatterAction::Select(id.to_string()));
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_new_chatter(move || {
            if let Some(window) = weak.upgrade()
                && !window.get_chatter_busy()
                && !window.get_chatter_dirty()
            {
                window.invoke_reset_chatter_draft();
                window.set_chatter_dirty(true);
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        window.on_discard_chatter(move || {
            if let Some(window) = weak.upgrade()
                && !window.get_chatter_busy()
                && let Ok(snapshot) = handle.snapshot.lock()
            {
                apply_selected(&window, snapshot.chatters.selected.as_ref());
                window.set_chatter_dirty(false);
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        window.on_save_chatter(move || {
            if let Some(window) = weak.upgrade()
                && window.get_chatter_ready()
                && window.get_chatter_dirty()
                && !window.get_chatter_busy()
            {
                let profile = handle
                    .snapshot
                    .lock()
                    .ok()
                    .and_then(|snapshot| save_profile(&window, &snapshot));
                let Some(profile) = profile else {
                    window.set_notice(
                        "The selected chatter changed. Select it again before saving.".into(),
                    );
                    return;
                };
                if submit(&window, &handle, ChatterAction::Save(profile)) {
                    window.set_chatter_busy(true);
                    window.set_notice("Saving chatter…".into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        window.on_delete_chatter(move || {
            if let Some(window) = weak.upgrade()
                && window.get_chatter_ready()
                && !window.get_chatter_dirty()
                && !window.get_chatter_busy()
            {
                let id = handle
                    .snapshot
                    .lock()
                    .ok()
                    .and_then(|snapshot| snapshot.chatters.selected.as_ref().map(|p| p.id.clone()))
                    .filter(|id| !id.is_empty());
                if let Some(id) = id
                    && submit(&window, &handle, ChatterAction::Delete(id))
                {
                    window.set_chatter_busy(true);
                    window.set_chatter_delete_confirm(false);
                    window.set_notice("Deleting chatter…".into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        window.on_forget_seen_chatter(move || {
            if let Some(window) = weak.upgrade()
                && !window.get_chatter_dirty()
                && !window.get_chatter_busy()
            {
                let user_id = handle.snapshot.lock().ok().and_then(|snapshot| {
                    snapshot
                        .chatters
                        .selected
                        .as_ref()
                        .and_then(|p| p.user_id.clone())
                });
                if let Some(user_id) = user_id
                    && submit(&window, &handle, ChatterAction::ForgetSeen(user_id))
                {
                    window.set_chatter_busy(true);
                    window.set_chatter_forget_confirm(false);
                    window.set_notice("Forgetting seen chatter…".into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let handle = handle.clone();
        window.on_clear_seen_chatters(move || {
            if let Some(window) = weak.upgrade()
                && !window.get_chatter_dirty()
                && !window.get_chatter_busy()
                && submit(&window, &handle, ChatterAction::ClearSeen)
            {
                window.set_chatter_busy(true);
                window.set_chatter_clear_confirm(false);
                window.set_notice("Clearing seen chatters…".into());
            }
        });
    }
}

pub(super) fn render(window: &DesktopWindow, view: &ChatterView) {
    window.set_chatter_ready(view.ready);
    window.set_chatter_ignored(view.selected_ignored);
    window.set_chatter_status(view.status.clone().into());
    window.set_chatter_error(view.error.clone().unwrap_or_default().into());
    window.set_chatter_total(view.total.min(i32::MAX as usize) as i32);
    window.set_chatter_seen_count(view.seen_count.min(i32::MAX as usize) as i32);
    window.set_chatter_profile_count(view.profile_count.min(i32::MAX as usize) as i32);
    window.set_chatter_estimated_bytes(view.estimated_bytes.min(i32::MAX as usize) as i32);
    window.set_chatter_query_id(view.query_id.min(i32::MAX as u64) as i32);
    window.set_chatter_selection_serial(view.selection_serial.min(i32::MAX as u64) as i32);
    window.set_chatter_action_serial(view.action_serial.min(i32::MAX as u64) as i32);

    let requested = window.get_chatter_requested_query_id().max(0) as u64;
    use slint::Model;
    let ids_model = window.get_chatter_row_ids();
    let labels_model = window.get_chatter_row_labels();
    let rows_changed = ids_model.row_count() != view.rows.len()
        || view.rows.iter().enumerate().any(|(i, row)| {
            ids_model.row_data(i).as_deref() != Some(row.id.as_str())
                || labels_model.row_data(i).as_deref() != Some(row.label.as_str())
        });
    if view.query_id == requested && rows_changed {
        let ids = view
            .rows
            .iter()
            .map(|row| row.id.clone().into())
            .collect::<Vec<slint::SharedString>>();
        let labels = view
            .rows
            .iter()
            .map(|row| row.label.clone().into())
            .collect::<Vec<slint::SharedString>>();
        window.set_chatter_row_ids(std::rc::Rc::new(slint::VecModel::from(ids)).into());
        window.set_chatter_row_labels(std::rc::Rc::new(slint::VecModel::from(labels)).into());
    }

    if view.action_serial.min(i32::MAX as u64) as i32 != window.get_chatter_last_action_serial() {
        window.set_chatter_last_action_serial(view.action_serial.min(i32::MAX as u64) as i32);
        window.set_chatter_busy(false);
        if view.error.is_none() {
            window.set_chatter_dirty(false);
            window.set_notice("Chatter changes saved.".into());
        } else {
            window.set_notice(view.error.clone().unwrap_or_default().into());
        }
    }
    if !window.get_chatter_dirty()
        && !window.get_chatter_busy()
        && view.selection_serial.min(i32::MAX as u64) as i32
            != window.get_chatter_last_selection_serial()
    {
        window.set_chatter_last_selection_serial(view.selection_serial.min(i32::MAX as u64) as i32);
        apply_selected(window, view.selected.as_ref());
    }
}
