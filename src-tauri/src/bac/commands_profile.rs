//! Saved-profile workspace helpers and append transaction for BAC.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use super::*;
use bac_host_contracts::workspace::CatalogEntry;

pub(crate) fn catalog_entries(index: &ProfileIndex) -> Vec<CatalogEntry> {
    index
        .profiles
        .iter()
        .map(|profile| CatalogEntry {
            id: profile.id.clone(),
            is_local: profile.kind == "local",
        })
        .collect()
}

pub(crate) fn first_saved_document(windows: &[Value]) -> Option<Value> {
    workspace_from_first_snapshot_window(&json!({"windows":windows}))
}

pub(crate) fn saved_document(window: &Value) -> Option<Value> {
    workspace_from_first_snapshot_window(&json!({"windows":[window]}))
}
