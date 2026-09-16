//! Shared chatter data. No transcript or credentials live in these types.
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Styles {
    pub sarcastic: bool,
    pub praise: bool,
    pub hero: bool,
    pub regular: bool,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatterProfile {
    pub id: String,
    pub user_id: Option<String>,
    pub login: String,
    pub nickname: String,
    pub description: String,
    pub styles: Styles,
    pub never_respond: bool,
    pub revision: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeenChatter {
    pub user_id: String,
    pub login: String,
    pub last_seen_at: u64,
    pub last_seen_channel_id: String,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ChatterFilter {
    #[default]
    Saved,
    Seen,
    Denied,
}

#[derive(Clone, Default)]
pub struct ChatterRow {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Default)]
pub struct ChatterView {
    pub ready: bool,
    pub rows: Vec<ChatterRow>,
    pub total: usize,
    pub query_id: u64,
    pub selected: Option<ChatterProfile>,
    pub selected_ignored: bool,
    pub selection_serial: u64,
    pub action_serial: u64,
    pub error: Option<String>,
    pub status: String,
    pub seen_count: usize,
    pub profile_count: usize,
    pub estimated_bytes: usize,
}

pub enum ChatterAction {
    Query {
        filter: ChatterFilter,
        search: String,
        offset: usize,
        query_id: u64,
    },
    Select(String),
    Save(ChatterProfile),
    Delete(String),
    ForgetSeen(String),
    ClearSeen,
}
