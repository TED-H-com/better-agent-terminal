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
// Used once bac:workspace-add-terminal is wired into the remote dispatcher.
#![cfg_attr(not(test), allow(dead_code))]

use super::*;
use bac_host_contracts::workspace::{
    self, empty_workspace_document, is_reserved_profile_id, prepare_append,
    read_bounded_saved_file, require_local_profile, saved_windows_append_eligible, verify_readback,
    AppendRequest, AppendResult, CatalogEntry, OperationError, SavedFileOutcome, Source,
    StagedAppend, TxAbort,
};
use std::{cell::Cell, panic::AssertUnwindSafe};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    CatalogRecheck,
    SnapshotRead,
    SnapshotWrite,
    Readback,
    IndexCommit,
}

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

fn abort(abort: TxAbort) -> io::Error {
    io::Error::other(abort)
}

fn refuse(error: OperationError) -> io::Error {
    abort(TxAbort::Refused(error))
}

pub(crate) fn append_persisted(
    dir: &Path,
    host_id: &str,
    request: &AppendRequest,
    overlay: &mut dyn FnMut(&mut Value),
) -> Result<StagedAppend, OperationError> {
    transact(dir, host_id, request, overlay, cfg!(windows), |_| Ok(()))
}

#[cfg(test)]
fn transact_with_faults(
    dir: &Path,
    host_id: &str,
    request: &AppendRequest,
    overlay: &mut dyn FnMut(&mut Value),
    windows: bool,
    point: impl FnMut(Point) -> io::Result<()>,
) -> Result<StagedAppend, OperationError> {
    transact(dir, host_id, request, overlay, windows, point)
}

fn transact(
    dir: &Path,
    host_id: &str,
    request: &AppendRequest,
    overlay: &mut dyn FnMut(&mut Value),
    windows: bool,
    mut point: impl FnMut(Point) -> io::Result<()>,
) -> Result<StagedAppend, OperationError> {
    if is_reserved_profile_id(&request.profile_id, windows) {
        return Err(OperationError::WorkspaceUnavailable);
    }
    let write_started = Cell::new(false);
    let outcome = update_index_at(dir, |index| {
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            point(Point::CatalogRecheck)?;
            workspace::catalog_files_acceptable(dir).map_err(refuse)?;
            require_local_profile(&catalog_entries(index), &request.profile_id).map_err(refuse)?;

            point(Point::SnapshotRead)?;
            let path = profile_path(dir, &request.profile_id);
            let document = match read_bounded_saved_file(&path, migrate_snapshot)
                .map_err(|_| refuse(OperationError::WorkspaceUnavailable))?
            {
                SavedFileOutcome::NoSavedWindows => empty_workspace_document(),
                SavedFileOutcome::Snapshot { windows, .. } => {
                    let document = first_saved_document(&windows)
                        .ok_or_else(|| refuse(OperationError::WorkspaceUnavailable))?;
                    if !saved_windows_append_eligible(&windows, &document) {
                        return Err(refuse(OperationError::WorkspaceUnavailable));
                    }
                    document
                }
            };

            let prospective = prepare_append(
                host_id,
                request,
                Source::Persisted,
                None,
                &document,
                overlay,
            )
            .map_err(abort)?;

            let entry = index
                .profiles
                .iter()
                .find(|profile| profile.id == request.profile_id)
                .cloned()
                .ok_or_else(|| refuse(OperationError::WorkspaceUnavailable))?;
            write_started.set(true);
            point(Point::SnapshotWrite)?;
            write_snapshot_at(
                dir,
                &request.profile_id,
                &snapshot_from_workspace(&entry, prospective.clone()),
            )?;

            point(Point::Readback)?;
            let readback = match read_bounded_saved_file(&path, migrate_snapshot)
                .map_err(|_| refuse(OperationError::WriteFailed))?
            {
                SavedFileOutcome::Snapshot { windows, .. } if windows.len() == 1 => {
                    first_saved_document(&windows)
                        .ok_or_else(|| refuse(OperationError::WriteFailed))?
                }
                _ => return Err(refuse(OperationError::WriteFailed)),
            };
            let revision =
                verify_readback(&readback, &prospective, &request.terminal.id).map_err(refuse)?;
            let document = Some(
                serde_json::to_string_pretty(&readback)
                    .map_err(|_| refuse(OperationError::WriteFailed))?,
            );
            let _ = activate_profile_in_index(index, &request.profile_id);
            point(Point::IndexCommit)?;
            Ok(StagedAppend {
                result: AppendResult {
                    host_id: host_id.to_owned(),
                    profile_id: request.profile_id.clone(),
                    source: Source::Persisted,
                    window_id: None,
                    terminal_id: request.terminal.id.clone(),
                    appended: true,
                    revision,
                },
                document,
                profile_changed: true,
            })
        }))
        .unwrap_or_else(|_| {
            Err(refuse(if write_started.get() {
                OperationError::WriteFailed
            } else {
                OperationError::Internal
            }))
        })
    });

    match outcome {
        Ok(staged) => Ok(staged),
        Err(err) => match err.get_ref().and_then(|e| e.downcast_ref::<TxAbort>()) {
            Some(TxAbort::Noop(result)) => Ok(StagedAppend {
                result: result.clone(),
                document: None,
                profile_changed: false,
            }),
            Some(TxAbort::Refused(_)) if write_started.get() => Err(OperationError::WriteFailed),
            Some(TxAbort::Refused(error)) => Err(error.clone()),
            None if write_started.get() => Err(OperationError::WriteFailed),
            None => Err(OperationError::WorkspaceUnavailable),
        },
    }
}
