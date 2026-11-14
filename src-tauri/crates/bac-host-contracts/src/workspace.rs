//! Shared, read-only BAC workspace contracts.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{self, Read},
    path::Path,
    time::Duration,
};

pub use crate::wire::{
    LOCATION_MATCHES_LIMIT, PROFILES_LIMIT, REGISTRATION_BYTES, REQUEST_BYTES, RESULT_BYTES,
    TERMINALS_LIMIT,
};
pub const DOCUMENT_MAX_BYTES: usize = 520_192;
pub const SAVED_FILE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const CATALOG_FILE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const SAVED_TOTAL_MAX_BYTES: usize = 64 * 1024 * 1024;
pub const WINDOW_ID_JSON_BYTES: usize = 1_024;
pub const SAVED_READ_MAX_ATTEMPTS: usize = 3;
pub const SAVED_READ_RETRY_DELAY: Duration = Duration::from_millis(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    pub profile_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocateRequest {
    pub session_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    WorkspaceRead(ReadRequest),
    SessionLocate(LocateRequest),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Live,
    Persisted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    Located,
    Ambiguous,
    Unregistered,
}
#[derive(Clone, Debug)]
pub struct CatalogEntry {
    pub id: String,
    pub is_local: bool,
}
#[derive(Clone, Debug)]
pub struct RegistryWindowSnapshot {
    pub id: String,
    pub profile_id: String,
    pub detached_workspace_id: Option<String>,
    pub has_content: bool,
    pub last_active_at: i64,
    pub workspace_document: Value,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocateRow {
    pub profile_id: String,
    pub workspace_id: String,
    pub terminal_id: String,
    pub window_id: Option<String>,
    pub profile_revision: String,
    pub source: Source,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub host_id: String,
    pub profile_id: String,
    pub source: Source,
    pub window_id: Option<String>,
    pub revision: String,
    pub document: Value,
    pub append_target_eligible: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocateResult {
    pub host_id: String,
    pub location_revision: String,
    pub disposition: Disposition,
    pub consistency: &'static str,
    pub atomic: bool,
    pub locations: Vec<LocateRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationError {
    InvalidRequest(&'static str),
    ProfileNotFound,
    ProfileNotLocal,
    WorkspaceUnavailable,
    WorkspaceTooLarge,
    LocationUnavailable(Vec<String>),
    LocationTooLarge,
    Internal,
}
impl OperationError {
    pub fn reply(&self) -> Value {
        let (code, message, details) = match self {
            Self::InvalidRequest(field) => (
                "INVALID_REQUEST",
                "Invalid BAC request",
                json!({"field":field}),
            ),
            Self::ProfileNotFound => ("PROFILE_NOT_FOUND", "Local profile not found", json!({})),
            Self::ProfileNotLocal => (
                "PROFILE_NOT_LOCAL",
                "BAC requires a local profile",
                json!({}),
            ),
            Self::WorkspaceUnavailable => (
                "WORKSPACE_UNAVAILABLE",
                "Canonical workspace is unavailable",
                json!({}),
            ),
            Self::WorkspaceTooLarge => (
                "WORKSPACE_TOO_LARGE",
                "Canonical workspace exceeds limit",
                json!({"maxBytes":DOCUMENT_MAX_BYTES}),
            ),
            Self::LocationUnavailable(ids) => (
                "LOCATION_UNAVAILABLE",
                "Session locations are unavailable",
                json!({"profileIds":ids}),
            ),
            Self::LocationTooLarge => (
                "LOCATION_TOO_LARGE",
                "Session locations exceed limit",
                json!({}),
            ),
            Self::Internal => ("INTERNAL_ERROR", "BAC request failed", json!({})),
        };
        crate::wire::error_reply(code, message, false, details)
    }
}

pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

pub fn valid_profile_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(ch, '_' | '.' | '-')
                || ('\u{4e00}'..='\u{9fff}').contains(&ch)
        })
}

pub fn is_reserved_profile_id(profile_id: &str, windows: bool) -> bool {
    if !windows {
        return false;
    }
    let stem = profile_id.split('.').next().unwrap_or(profile_id);
    if stem.eq_ignore_ascii_case("CON")
        || stem.eq_ignore_ascii_case("PRN")
        || stem.eq_ignore_ascii_case("AUX")
        || stem.eq_ignore_ascii_case("NUL")
    {
        return true;
    }
    match stem.as_bytes() {
        [a, b, c, d] => matches!(
            (
                a.to_ascii_uppercase(),
                b.to_ascii_uppercase(),
                c.to_ascii_uppercase(),
                *d
            ),
            (b'C', b'O', b'M', b'0'..=b'9') | (b'L', b'P', b'T', b'0'..=b'9')
        ),
        _ => false,
    }
}

pub fn window_id_within_bound(id: &str) -> bool {
    serde_json::to_string(id).is_ok_and(|encoded| encoded.len() <= WINDOW_ID_JSON_BYTES)
}

pub fn parse_request(channel: &str, params: &Value) -> Result<Request, OperationError> {
    let object = params
        .as_object()
        .ok_or(OperationError::InvalidRequest("params"))?;
    let (key, field, validator): (_, _, fn(&str) -> bool) = match channel {
        "bac:workspace-read" => ("profileId", "params.profileId", valid_profile_id),
        "bac:session-locate" => ("sessionId", "params.sessionId", valid_id),
        _ => return Err(OperationError::InvalidRequest("params")),
    };
    if object.keys().any(|name| name != key) {
        return Err(OperationError::InvalidRequest("params"));
    }
    let id = object
        .get(key)
        .and_then(Value::as_str)
        .filter(|id| validator(id))
        .ok_or(OperationError::InvalidRequest(field))?
        .to_owned();
    match channel {
        "bac:workspace-read" => Ok(Request::WorkspaceRead(ReadRequest { profile_id: id })),
        "bac:session-locate" => Ok(Request::SessionLocate(LocateRequest { session_id: id })),
        _ => Err(OperationError::InvalidRequest("params")),
    }
}

pub fn require_local_profile(
    catalog: &[CatalogEntry],
    profile_id: &str,
) -> Result<(), OperationError> {
    let entry = catalog
        .iter()
        .find(|entry| entry.id == profile_id && valid_profile_id(&entry.id))
        .ok_or(OperationError::ProfileNotFound)?;
    if !entry.is_local {
        return Err(OperationError::ProfileNotLocal);
    }
    Ok(())
}

/// Resolve each catalog ID from its first entry, keep locals, then check grammar and limit.
pub fn local_catalog_profiles(
    catalog: &[CatalogEntry],
) -> Result<Vec<&CatalogEntry>, OperationError> {
    let mut seen = HashSet::new();
    let locals: Vec<&CatalogEntry> = catalog
        .iter()
        .filter(|entry| seen.insert(entry.id.as_str()) && entry.is_local)
        .collect();
    if locals.iter().any(|profile| !valid_profile_id(&profile.id)) {
        return Err(OperationError::LocationUnavailable(vec![]));
    }
    if locals.len() > PROFILES_LIMIT {
        return Err(OperationError::LocationTooLarge);
    }
    Ok(locals)
}

pub fn canonicalize_json(value: &Value) -> Result<Value, OperationError> {
    match value {
        Value::Array(values) => values
            .iter()
            .map(canonicalize_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => {
            let sorted = values
                .iter()
                .map(|(key, value)| Ok((key.clone(), canonicalize_json(value)?)))
                .collect::<Result<BTreeMap<_, _>, OperationError>>()?;
            Ok(Value::Object(sorted.into_iter().collect::<Map<_, _>>()))
        }
        _ => Ok(value.clone()),
    }
}

pub fn canonical_digest(doc: &Value) -> Result<(String, usize), OperationError> {
    let bytes =
        serde_json::to_vec(&canonicalize_json(doc)?).map_err(|_| OperationError::Internal)?;
    Ok((format!("sha256:{:x}", Sha256::digest(&bytes)), bytes.len()))
}

pub fn structural_preconditions_ok(doc: &Value) -> bool {
    ["workspaces", "terminals"].into_iter().all(|key| {
        let Some(items) = doc.get(key).and_then(Value::as_array) else {
            return false;
        };
        let mut ids = HashSet::new();
        items.iter().all(|item| {
            item.as_object().is_some()
                && item
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty() && ids.insert(id))
        })
    })
}

pub fn select_regular_entry<'a>(
    entries: &'a [RegistryWindowSnapshot],
    profile_id: &str,
) -> Option<&'a RegistryWindowSnapshot> {
    entries
        .iter()
        .filter(|entry| {
            entry.profile_id == profile_id
                && entry.detached_workspace_id.is_none()
                && entry.has_content
        })
        .max_by_key(|entry| entry.last_active_at)
}

#[derive(Clone, Debug)]
pub enum SavedFileOutcome {
    NoSavedWindows,
    Snapshot {
        raw_bytes: usize,
        windows: Vec<Value>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedReadError {
    TooLarge,
    Unreadable,
}

/// Parse and migrate the same bounded byte buffer; never reopen it for migration.
pub fn read_bounded_saved_file(
    path: &Path,
    migrate: impl FnMut(Value) -> Option<Value>,
) -> Result<SavedFileOutcome, SavedReadError> {
    read_bounded_saved_file_measured(path, migrate).0
}

/// Also report the final observed byte count, including empty or unreadable files,
/// so a locate scan enforces its cumulative bound before reporting unreadability.
fn open_regular_saved_file(path: &Path) -> Result<File, Result<SavedFileOutcome, SavedReadError>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(Ok(SavedFileOutcome::NoSavedWindows))
        }
        Err(_) => return Err(Err(SavedReadError::Unreadable)),
    };
    let Ok(meta) = file.metadata() else {
        return Err(Err(SavedReadError::Unreadable));
    };
    if !meta.is_file() {
        return Err(Err(SavedReadError::Unreadable));
    }
    Ok(file)
}

pub fn read_bounded_saved_file_measured(
    path: &Path,
    migrate: impl FnMut(Value) -> Option<Value>,
) -> (Result<SavedFileOutcome, SavedReadError>, usize) {
    read_bounded_saved_file_measured_for_platform(path, cfg!(windows), migrate)
}

fn read_bounded_saved_file_measured_for_platform(
    path: &Path,
    windows: bool,
    mut migrate: impl FnMut(Value) -> Option<Value>,
) -> (Result<SavedFileOutcome, SavedReadError>, usize) {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| is_reserved_profile_id(name, windows))
    {
        return (Err(SavedReadError::Unreadable), 0);
    }
    let mut last_size = 0;
    for attempt in 0..SAVED_READ_MAX_ATTEMPTS {
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return (Ok(SavedFileOutcome::NoSavedWindows), 0)
            }
            Err(_) => return (Err(SavedReadError::Unreadable), 0),
        };
        if !meta.is_file() {
            return (Err(SavedReadError::Unreadable), 0);
        }
        if meta.len() > SAVED_FILE_MAX_BYTES as u64 {
            return (Err(SavedReadError::TooLarge), 0);
        }
        let file = match open_regular_saved_file(path) {
            Ok(file) => file,
            Err(outcome) => return (outcome, 0),
        };
        let mut raw = Vec::new();
        if file
            .take((SAVED_FILE_MAX_BYTES + 1) as u64)
            .read_to_end(&mut raw)
            .is_err()
        {
            return (Err(SavedReadError::Unreadable), raw.len());
        }
        last_size = raw.len();
        if last_size > SAVED_FILE_MAX_BYTES {
            return (Err(SavedReadError::TooLarge), last_size);
        }
        let snapshot = serde_json::from_slice::<Value>(&raw)
            .ok()
            .and_then(&mut migrate);
        if let Some(snapshot) = snapshot {
            let Some(windows) = snapshot.get("windows").and_then(Value::as_array) else {
                return (Err(SavedReadError::Unreadable), last_size);
            };
            let result = if windows.is_empty() {
                SavedFileOutcome::NoSavedWindows
            } else {
                SavedFileOutcome::Snapshot {
                    raw_bytes: last_size,
                    windows: windows.clone(),
                }
            };
            return (Ok(result), last_size);
        }
        if attempt + 1 < SAVED_READ_MAX_ATTEMPTS {
            std::thread::sleep(SAVED_READ_RETRY_DELAY);
        }
    }
    (Err(SavedReadError::Unreadable), last_size)
}

/// Assemble one read from an already-observed registry and a lazy persisted read.
pub fn execute_read(
    host_id: &str,
    request: &ReadRequest,
    catalog: &[CatalogEntry],
    entries: &[RegistryWindowSnapshot],
    load_saved: impl FnOnce() -> Result<SavedFileOutcome, SavedReadError>,
    first_document: impl FnOnce(&[Value]) -> Option<Value>,
) -> Result<ReadResult, OperationError> {
    require_local_profile(catalog, &request.profile_id)?;
    if entries
        .iter()
        .any(|entry| entry.profile_id == request.profile_id && !window_id_within_bound(&entry.id))
    {
        return Err(OperationError::WorkspaceUnavailable);
    }
    let (document, source, window_id, eligible) =
        if let Some(entry) = select_regular_entry(entries, &request.profile_id) {
            let document = entry.workspace_document.clone();
            let eligible = structural_preconditions_ok(&document);
            (document, Source::Live, Some(entry.id.clone()), eligible)
        } else {
            match load_saved().map_err(|_| OperationError::WorkspaceUnavailable)? {
                SavedFileOutcome::NoSavedWindows => {
                    (empty_workspace_document(), Source::Persisted, None, false)
                }
                SavedFileOutcome::Snapshot { windows, .. } => {
                    let eligible_window_count = windows.len() == 1;
                    let document =
                        first_document(&windows).ok_or(OperationError::WorkspaceUnavailable)?;
                    let eligible = eligible_window_count && structural_preconditions_ok(&document);
                    (document, Source::Persisted, None, eligible)
                }
            }
        };
    let (revision, bytes) = canonical_digest(&document)?;
    if bytes > DOCUMENT_MAX_BYTES {
        return Err(OperationError::WorkspaceTooLarge);
    }
    Ok(ReadResult {
        host_id: host_id.to_owned(),
        profile_id: request.profile_id.clone(),
        source,
        window_id,
        revision,
        document,
        append_target_eligible: eligible,
    })
}

pub fn empty_workspace_document() -> Value {
    json!({"workspaces": [], "activeWorkspaceId": null, "activeGroup": null, "terminals": [], "activeTerminalId": null})
}

#[derive(Clone, Debug)]
pub struct SavedProfile {
    pub profile_id: String,
    pub outcome: SavedFileOutcome,
}

struct ObservedDocument {
    profile_id: String,
    source: Source,
    window_id: Option<String>,
    detached_workspace_id: Option<String>,
    document: Value,
}

fn locate_document_is_readable(document: &Value) -> bool {
    ["workspaces", "terminals"].into_iter().all(|key| {
        document
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().all(|item| {
                    item.as_object().is_some()
                        && item
                            .get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| !id.is_empty())
                })
            })
    })
}

pub fn execute_locate(
    host_id: &str,
    request: &LocateRequest,
    catalog: &[CatalogEntry],
    entries: &[RegistryWindowSnapshot],
    saved: &[SavedProfile],
    mut unavailable: Vec<String>,
    saved_document: impl Fn(&Value) -> Option<Value>,
) -> Result<LocateResult, OperationError> {
    use std::collections::{BTreeMap, BTreeSet};
    let locals = local_catalog_profiles(catalog)?;
    let local_ids: HashSet<&str> = locals.iter().map(|profile| profile.id.as_str()).collect();
    let mut documents = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| local_ids.contains(entry.profile_id.as_str()))
    {
        if !window_id_within_bound(&entry.id)
            || !locate_document_is_readable(&entry.workspace_document)
        {
            unavailable.push(entry.profile_id.clone());
        }
        documents.push(ObservedDocument {
            profile_id: entry.profile_id.clone(),
            source: Source::Live,
            window_id: Some(entry.id.clone()),
            detached_workspace_id: entry.detached_workspace_id.clone(),
            document: entry.workspace_document.clone(),
        });
    }
    for profile in saved
        .iter()
        .filter(|profile| local_ids.contains(profile.profile_id.as_str()))
    {
        if let SavedFileOutcome::Snapshot { windows, .. } = &profile.outcome {
            for window in windows {
                let Some(document) = saved_document(window) else {
                    unavailable.push(profile.profile_id.clone());
                    continue;
                };
                if !locate_document_is_readable(window) || !locate_document_is_readable(&document) {
                    unavailable.push(profile.profile_id.clone());
                }
                documents.push(ObservedDocument {
                    profile_id: profile.profile_id.clone(),
                    source: Source::Persisted,
                    window_id: None,
                    detached_workspace_id: None,
                    document,
                });
            }
        }
    }
    if !unavailable.is_empty() {
        unavailable.sort();
        unavailable.dedup();
        return Err(OperationError::LocationUnavailable(unavailable));
    }
    let terminal_count = documents
        .iter()
        .map(|observed| {
            observed
                .document
                .get("terminals")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .sum::<usize>();
    if terminal_count > TERMINALS_LIMIT {
        return Err(OperationError::LocationTooLarge);
    }

    type Pair = (String, String, String);
    type Placement = (String, String, String, Option<String>, Source);
    let mut regular_pairs: HashSet<Pair> = HashSet::new();
    let mut candidates = Vec::<Placement>::new();
    for observed in &documents {
        let workspaces: HashSet<&str> = observed
            .document
            .get("workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|workspace| workspace.get("id").and_then(Value::as_str))
            .collect();
        let terminals = observed
            .document
            .get("terminals")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for terminal in terminals {
            let Some(workspace) = terminal.get("workspaceId").and_then(Value::as_str) else {
                continue;
            };
            let Some(id) = terminal.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !workspaces.contains(workspace)
                || observed
                    .detached_workspace_id
                    .as_deref()
                    .is_some_and(|detached| detached != workspace)
            {
                continue;
            }
            let pair = (
                observed.profile_id.clone(),
                workspace.to_owned(),
                id.to_owned(),
            );
            if observed.source == Source::Live && observed.detached_workspace_id.is_none() {
                regular_pairs.insert(pair.clone());
            }
            candidates.push((
                pair.0,
                pair.1,
                pair.2,
                observed.window_id.clone(),
                observed.source,
            ));
        }
    }
    let placements: BTreeSet<Placement> = candidates
        .into_iter()
        .filter(|(profile, workspace, terminal, _, source)| {
            terminal == &request.session_id
                && (*source != Source::Persisted
                    || !regular_pairs.contains(&(
                        profile.clone(),
                        workspace.clone(),
                        terminal.clone(),
                    )))
        })
        .collect();
    if placements.len() > LOCATION_MATCHES_LIMIT {
        return Err(OperationError::LocationTooLarge);
    }
    let mut profile_revisions = BTreeMap::<String, String>::new();
    let mut locations = Vec::new();
    for (profile_id, workspace_id, terminal_id, window_id, source) in placements {
        let revision = if let Some(value) = profile_revisions.get(&profile_id) {
            value.clone()
        } else {
            let document = select_regular_entry(entries, &profile_id)
                .map(|entry| entry.workspace_document.clone())
                .or_else(|| {
                    documents
                        .iter()
                        .find(|doc| doc.profile_id == profile_id && doc.source == Source::Persisted)
                        .map(|doc| doc.document.clone())
                })
                .unwrap_or_else(empty_workspace_document);
            let revision = canonical_digest(&document)?.0;
            profile_revisions.insert(profile_id.clone(), revision.clone());
            revision
        };
        locations.push(LocateRow {
            profile_id,
            workspace_id,
            terminal_id,
            window_id,
            source,
            profile_revision: revision,
        });
    }
    let mut hashes = Vec::new();
    for observed in &documents {
        hashes.push((
            observed.profile_id.clone(),
            observed.source,
            observed.window_id.clone(),
            canonical_digest(&observed.document)?.0,
        ));
    }
    hashes.sort();
    let hashes = serde_json::to_value(hashes).map_err(|_| OperationError::Internal)?;
    let location_revision = canonical_digest(&hashes)?.0;
    let disposition = match locations.len() {
        0 => Disposition::Unregistered,
        1 => Disposition::Located,
        _ => Disposition::Ambiguous,
    };
    let result = LocateResult {
        host_id: host_id.to_owned(),
        location_revision,
        disposition,
        consistency: "sequential",
        atomic: false,
        locations,
    };
    if serde_json::to_vec(&result)
        .map_err(|_| OperationError::Internal)?
        .len()
        > DOCUMENT_MAX_BYTES
    {
        return Err(OperationError::LocationTooLarge);
    }
    Ok(result)
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
