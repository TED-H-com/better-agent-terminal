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

#[cfg(test)]
mod tests {
    use super::*;
    use bac_host_contracts::workspace::{
        canonical_digest, execute_read, normalize_registration, AgentPreset, ReadRequest,
        Registration, CATALOG_FILE_MAX_BYTES,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    const HOST: &str = "h_0123456789abcdef0123456789abcdef";

    fn temp_profile_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bac-profile-append-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_registration(id: &str, workspace_id: &str) -> Registration {
        Registration {
            id: id.into(),
            workspace_id: workspace_id.into(),
            title: "Agent".into(),
            cwd: "/repo".into(),
            agent_preset: AgentPreset::ClaudeCode,
            model: None,
            permission_mode: None,
            agent_params: None,
            worktree_path: None,
            worktree_branch: None,
        }
    }

    fn write_index(dir: &Path, profiles: Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(&json!({
            "profiles": profiles,
            "activeProfileIds": [],
        }))
        .unwrap();
        fs::write(dir.join("index.json"), &bytes).unwrap();
        bytes
    }

    #[test]
    fn persisted_append_no_saved_windows_stops_with_workspace_not_found() {
        let dir = temp_profile_dir("no-saved-windows");
        let index_before = write_index(
            &dir,
            json!([
                {"id": "local", "name": "Local", "type": "local", "createdAt": 0, "updatedAt": 0}
            ]),
        );
        let empty_rev = canonical_digest(&empty_workspace_document()).unwrap().0;
        let request = AppendRequest {
            profile_id: "local".into(),
            expected_revision: empty_rev,
            terminal: sample_registration("t1", "w1"),
        };
        let path = profile_path(&dir, "local");

        assert!(!path.exists());
        assert_eq!(
            append_persisted(&dir, HOST, &request, &mut |_| {}),
            Err(OperationError::WorkspaceNotFound("w1".into()))
        );
        assert!(!path.exists());
        assert_eq!(fs::read(dir.join("index.json")).unwrap(), index_before);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("missing-target.json"), &path).unwrap();
            assert_eq!(
                append_persisted(&dir, HOST, &request, &mut |_| {}),
                Err(OperationError::WorkspaceNotFound("w1".into()))
            );
            assert!(fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(!path.exists());
            assert_eq!(fs::read(dir.join("index.json")).unwrap(), index_before);
            fs::remove_file(&path).unwrap();
        }

        let empty_windows = br#"{"version":2,"windows":[]}"#;
        fs::write(&path, empty_windows).unwrap();
        assert_eq!(
            append_persisted(&dir, HOST, &request, &mut |_| {}),
            Err(OperationError::WorkspaceNotFound("w1".into()))
        );
        assert_eq!(fs::read(&path).unwrap(), empty_windows);
        assert_eq!(fs::read(dir.join("index.json")).unwrap(), index_before);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persisted_append_refuses_every_snapshot_the_read_reports_ineligible() {
        let dir = temp_profile_dir("ineligible");
        write_index(
            &dir,
            json!([
                {"id": "local", "name": "Local", "type": "local", "createdAt": 0, "updatedAt": 0}
            ]),
        );
        let index = read_catalog_unlocked(&dir).unwrap();
        let catalog = catalog_entries(&index);
        let path = profile_path(&dir, "local");

        let read_profile = || {
            execute_read(
                HOST,
                &ReadRequest {
                    profile_id: "local".into(),
                },
                &catalog,
                &[],
                || read_bounded_saved_file(&path, migrate_snapshot),
                first_saved_document,
            )
            .unwrap()
        };

        let ineligible_snapshots = [
            json!({
                "version": 2,
                "windows": [
                    {"workspaces": [{"id": "w1"}], "terminals": []},
                    {"workspaces": [{"id": "w2"}], "terminals": []}
                ]
            }),
            json!({
                "version": 2,
                "windows": [
                    {"workspaces": [{"id": "w1"}], "terminals": [{"workspaceId": "w1"}]}
                ]
            }),
            json!({
                "version": 2,
                "windows": [
                    {"workspaces": "not-an-array", "terminals": []}
                ]
            }),
            json!({
                "version": 2,
                "windows": [
                    {
                        "workspaces": [{"id": "w1"}],
                        "terminals": [
                            {"id": "dup", "workspaceId": "w1"},
                            {"id": "dup", "workspaceId": "w1"}
                        ]
                    }
                ]
            }),
        ];

        for snapshot in ineligible_snapshots {
            let raw_bytes = serde_json::to_vec(&snapshot).unwrap();
            fs::write(&path, &raw_bytes).unwrap();
            let read_result = read_profile();
            assert!(!read_result.append_target_eligible);
            let request = AppendRequest {
                profile_id: "local".into(),
                expected_revision: read_result.revision,
                terminal: sample_registration("t1", "w1"),
            };
            assert_eq!(
                append_persisted(&dir, HOST, &request, &mut |_| {}),
                Err(OperationError::WorkspaceUnavailable)
            );
            assert_eq!(fs::read(&path).unwrap(), raw_bytes);
        }

        let eligible_snapshot = json!({
            "version": 2,
            "windows": [
                {"workspaces": [{"id": "w1"}], "terminals": []}
            ]
        });
        fs::write(&path, serde_json::to_vec(&eligible_snapshot).unwrap()).unwrap();
        let read_result = read_profile();
        assert!(read_result.append_target_eligible);
        let request = AppendRequest {
            profile_id: "local".into(),
            expected_revision: read_result.revision,
            terminal: sample_registration("t1", "w1"),
        };
        let staged = append_persisted(&dir, HOST, &request, &mut |_| {}).unwrap();
        assert!(staged.result.appended);
        assert!(staged.profile_changed);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persisted_append_writes_readback_and_activates_profile() {
        let dir = temp_profile_dir("writes-and-activates");
        write_index(
            &dir,
            json!([
                {"id": "local", "name": "Local", "type": "local", "createdAt": 0, "updatedAt": 0},
                {"id": "remote", "name": "Remote", "type": "remote", "remoteToken": "fixture-only", "createdAt": 0, "updatedAt": 0},
                {"id": "shadowed", "name": "Shadowed Remote", "type": "remote", "remoteToken": "fixture-only", "createdAt": 0, "updatedAt": 0},
                {"id": "shadowed", "name": "Shadowed Local", "type": "local", "createdAt": 0, "updatedAt": 0},
                {"id": "CON", "name": "Reserved", "type": "local", "createdAt": 0, "updatedAt": 0}
            ]),
        );

        let initial_snapshot = json!({
            "version": 2,
            "windows": [
                {"workspaces": [{"id": "w1"}], "terminals": []}
            ]
        });
        let initial_bytes = serde_json::to_vec(&initial_snapshot).unwrap();
        for id in ["local", "shadowed", "CON"] {
            fs::write(profile_path(&dir, id), &initial_bytes).unwrap();
        }

        let base_doc =
            first_saved_document(initial_snapshot["windows"].as_array().unwrap()).unwrap();
        let initial_rev = canonical_digest(&base_doc).unwrap().0;
        let reg = sample_registration("t1", "w1");
        let request = AppendRequest {
            profile_id: "local".into(),
            expected_revision: initial_rev.clone(),
            terminal: reg.clone(),
        };

        let staged = append_persisted(&dir, HOST, &request, &mut |doc| {
            doc["activeTerminalId"] = json!("t1");
        })
        .unwrap();
        assert!(staged.result.appended);
        assert_eq!(staged.result.source, Source::Persisted);
        assert_eq!(staged.result.window_id, None);
        assert_eq!(staged.result.profile_id, "local");
        assert_eq!(staged.result.terminal_id, "t1");
        assert!(staged.profile_changed);

        let SavedFileOutcome::Snapshot { windows, .. } =
            read_bounded_saved_file(&profile_path(&dir, "local"), migrate_snapshot).unwrap()
        else {
            panic!("expected single-window snapshot after append");
        };
        let readback_doc = first_saved_document(&windows).unwrap();
        assert_eq!(readback_doc["terminals"][0], normalize_registration(&reg));
        assert_eq!(readback_doc["activeTerminalId"], json!("t1"));
        assert_eq!(
            staged.result.revision,
            canonical_digest(&readback_doc).unwrap().0
        );
        assert_eq!(
            staged.document,
            Some(serde_json::to_string_pretty(&readback_doc).unwrap())
        );

        let index_after: Value =
            serde_json::from_slice(&fs::read(dir.join("index.json")).unwrap()).unwrap();
        assert!(index_after["activeProfileIds"]
            .as_array()
            .unwrap()
            .contains(&json!("local")));

        let file_after_first = fs::read(profile_path(&dir, "local")).unwrap();
        let index_bytes_after_first = fs::read(dir.join("index.json")).unwrap();

        let retry = append_persisted(&dir, HOST, &request, &mut |doc| {
            doc["activeTerminalId"] = json!("t1");
        })
        .unwrap();
        assert!(!retry.result.appended);
        assert!(!retry.profile_changed);
        assert_eq!(retry.document, None);
        assert_eq!(retry.result.revision, staged.result.revision);
        assert_eq!(
            fs::read(profile_path(&dir, "local")).unwrap(),
            file_after_first
        );
        assert_eq!(
            fs::read(dir.join("index.json")).unwrap(),
            index_bytes_after_first
        );

        let mut conflicting_reg = reg.clone();
        conflicting_reg.cwd = "/different".into();
        let conflict_request = AppendRequest {
            profile_id: "local".into(),
            expected_revision: staged.result.revision.clone(),
            terminal: conflicting_reg,
        };
        assert_eq!(
            append_persisted(&dir, HOST, &conflict_request, &mut |_| {}),
            Err(OperationError::TerminalExists {
                terminal_id: "t1".into(),
                current_revision: staged.result.revision.clone(),
            })
        );

        let stale_request = AppendRequest {
            profile_id: "local".into(),
            expected_revision: initial_rev.clone(),
            terminal: sample_registration("t2", "w1"),
        };
        assert_eq!(
            append_persisted(&dir, HOST, &stale_request, &mut |_| {}),
            Err(OperationError::RevisionMismatch {
                current_revision: staged.result.revision.clone(),
            })
        );

        let unknown_request = AppendRequest {
            profile_id: "unknown".into(),
            expected_revision: initial_rev.clone(),
            terminal: sample_registration("t2", "w1"),
        };
        assert_eq!(
            append_persisted(&dir, HOST, &unknown_request, &mut |_| {}),
            Err(OperationError::ProfileNotFound)
        );

        let remote_request = AppendRequest {
            profile_id: "remote".into(),
            expected_revision: initial_rev.clone(),
            terminal: sample_registration("t2", "w1"),
        };
        assert_eq!(
            append_persisted(&dir, HOST, &remote_request, &mut |_| {}),
            Err(OperationError::ProfileNotLocal)
        );

        let shadowed_request = AppendRequest {
            profile_id: "shadowed".into(),
            expected_revision: initial_rev.clone(),
            terminal: sample_registration("t2", "w1"),
        };
        assert_eq!(
            append_persisted(&dir, HOST, &shadowed_request, &mut |_| {}),
            Err(OperationError::ProfileNotLocal)
        );

        let con_request = AppendRequest {
            profile_id: "CON".into(),
            expected_revision: initial_rev,
            terminal: sample_registration("t2", "w1"),
        };
        let con_bytes_before = fs::read(profile_path(&dir, "CON")).unwrap();
        let index_before_con = fs::read(dir.join("index.json")).unwrap();
        let mut reached = Vec::new();
        assert_eq!(
            transact_with_faults(&dir, HOST, &con_request, &mut |_| {}, true, |point| {
                reached.push(point);
                Ok(())
            }),
            Err(OperationError::WorkspaceUnavailable)
        );
        assert!(reached.is_empty());
        assert_eq!(
            fs::read(profile_path(&dir, "CON")).unwrap(),
            con_bytes_before
        );
        assert_eq!(fs::read(dir.join("index.json")).unwrap(), index_before_con);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persisted_append_fault_points_pre_and_post_write_and_unwind() {
        let dir = temp_profile_dir("fault-points");
        let initial_index_bytes = write_index(
            &dir,
            json!([
                {"id": "local", "name": "Local", "type": "local", "createdAt": 0, "updatedAt": 0}
            ]),
        );
        let initial_snapshot = json!({
            "version": 2,
            "windows": [
                {"workspaces": [{"id": "w1"}], "terminals": []}
            ]
        });
        let initial_profile_bytes = serde_json::to_vec(&initial_snapshot).unwrap();
        let path = profile_path(&dir, "local");
        fs::write(&path, &initial_profile_bytes).unwrap();

        let base_doc =
            first_saved_document(initial_snapshot["windows"].as_array().unwrap()).unwrap();
        let initial_rev = canonical_digest(&base_doc).unwrap().0;
        let request = AppendRequest {
            profile_id: "local".into(),
            expected_revision: initial_rev,
            terminal: sample_registration("t1", "w1"),
        };

        for target in [Point::CatalogRecheck, Point::SnapshotRead] {
            assert_eq!(
                transact_with_faults(&dir, HOST, &request, &mut |_| {}, false, |point| {
                    if point == target {
                        Err(io::Error::other("injected pre-write"))
                    } else {
                        Ok(())
                    }
                }),
                Err(OperationError::WorkspaceUnavailable)
            );
            assert_eq!(fs::read(&path).unwrap(), initial_profile_bytes);
            assert_eq!(
                fs::read(dir.join("index.json")).unwrap(),
                initial_index_bytes
            );
        }

        assert_eq!(
            transact_with_faults(&dir, HOST, &request, &mut |_| {}, false, |point| {
                if point == Point::CatalogRecheck {
                    fs::File::create(dir.join("index.json.bak"))
                        .unwrap()
                        .set_len(CATALOG_FILE_MAX_BYTES as u64 + 1)
                        .unwrap();
                }
                Ok(())
            }),
            Err(OperationError::WorkspaceUnavailable)
        );
        assert_eq!(fs::read(&path).unwrap(), initial_profile_bytes);
        assert_eq!(
            fs::read(dir.join("index.json")).unwrap(),
            initial_index_bytes
        );
        fs::remove_file(dir.join("index.json.bak")).unwrap();

        for target in [Point::SnapshotWrite, Point::Readback, Point::IndexCommit] {
            fs::write(&path, &initial_profile_bytes).unwrap();
            fs::write(dir.join("index.json"), &initial_index_bytes).unwrap();
            let err = transact_with_faults(&dir, HOST, &request, &mut |_| {}, false, |point| {
                if point == target {
                    Err(io::Error::other("injected post-write"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert_eq!(err, OperationError::WriteFailed);
            assert_eq!(
                err.reply()["error"]["details"],
                json!({"outcome": "unknown"})
            );
        }

        fs::write(&path, &initial_profile_bytes).unwrap();
        fs::write(dir.join("index.json"), &initial_index_bytes).unwrap();
        assert_eq!(
            transact_with_faults(&dir, HOST, &request, &mut |_| {}, false, |point| {
                if point == Point::Readback {
                    let tampered = json!({
                        "version": 2,
                        "windows": [{
                            "workspaces": [{"id": "w1"}],
                            "terminals": [{
                                "id": "t1",
                                "workspaceId": "w1",
                                "title": "Agent",
                                "type": "terminal",
                                "cwd": "/tampered",
                                "agentPreset": "claude-code"
                            }]
                        }]
                    });
                    fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
                }
                Ok(())
            }),
            Err(OperationError::WriteFailed)
        );

        fs::write(&path, &initial_profile_bytes).unwrap();
        fs::write(dir.join("index.json"), &initial_index_bytes).unwrap();
        assert_eq!(
            transact_with_faults(&dir, HOST, &request, &mut |_| {}, false, |point| {
                if point == Point::SnapshotRead {
                    panic!("injected pre-write panic");
                }
                Ok(())
            }),
            Err(OperationError::Internal)
        );

        assert_eq!(
            transact_with_faults(&dir, HOST, &request, &mut |_| {}, false, |point| {
                if point == Point::Readback {
                    panic!("injected post-write panic");
                }
                Ok(())
            }),
            Err(OperationError::WriteFailed)
        );

        fs::write(&path, &initial_profile_bytes).unwrap();
        fs::write(dir.join("index.json"), &initial_index_bytes).unwrap();
        let staged = append_persisted(&dir, HOST, &request, &mut |_| {}).unwrap();
        assert!(staged.result.appended);
        assert!(staged.profile_changed);

        fs::remove_dir_all(dir).unwrap();
    }
}
