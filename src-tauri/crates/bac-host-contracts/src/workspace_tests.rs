use super::*;

#[test]
fn canonical_digest_sorts_keys_and_matches_empty_workspace() {
    let left = json!({"z":{"b":1,"a":2},"a":[{"y":null,"x":true}]});
    let right: Value =
        serde_json::from_str(r#"{"a":[{"x":true,"y":null}],"z":{"a":2,"b":1}}"#).unwrap();
    assert_eq!(
        canonical_digest(&left).unwrap(),
        canonical_digest(&right).unwrap()
    );
    assert_ne!(
        canonical_digest(&json!({"a":null})).unwrap(),
        canonical_digest(&json!({})).unwrap()
    );
    let empty = json!({"workspaces":[],"activeWorkspaceId":null,"activeGroup":null,"terminals":[],"activeTerminalId":null});
    assert_eq!(
        canonical_digest(&empty).unwrap().0,
        "sha256:66c1b7a04a05ee220e10341c9baec59bdf92885b193c2838d527a235d2ca5a9c"
    );
}

#[test]
fn request_ids_and_static_error_fields_are_checked() {
    for bad in ["", "bad/id", "has space", "é", &"a".repeat(129)] {
        assert!(!valid_id(bad));
        assert_eq!(
            parse_request("bac:workspace-read", &json!({"profileId":bad})),
            Err(OperationError::InvalidRequest("params.profileId"))
        );
    }
    assert!(valid_id("A-1_.:z"));
    assert_eq!(
        parse_request(
            "bac:session-locate",
            &json!({"sessionId":"s","unexpected":true})
        ),
        Err(OperationError::InvalidRequest("params"))
    );
    assert_eq!(
        OperationError::WorkspaceTooLarge.reply()["error"]["details"],
        json!({"maxBytes":520192})
    );
}

#[test]
fn window_id_json_bound_counts_escaping() {
    assert!(window_id_within_bound(&"w".repeat(1022)));
    assert!(!window_id_within_bound(&"w".repeat(1023)));
    assert!(!window_id_within_bound(&"\u{1}".repeat(171)));
}

const HOST: &str = "h_0123456789abcdef0123456789abcdef";

fn catalog(id: &str) -> Vec<CatalogEntry> {
    vec![CatalogEntry {
        id: id.into(),
        is_local: true,
    }]
}
fn document(workspaces: Value, terminals: Value) -> Value {
    json!({"workspaces":workspaces,"activeWorkspaceId":null,"activeGroup":null,"terminals":terminals,"activeTerminalId":null})
}
fn saved_document(window: &Value) -> Option<Value> {
    Some(json!({
        "workspaces":window.get("workspaces").cloned().unwrap_or_else(|| json!([])),
        "activeWorkspaceId":window.get("activeWorkspaceId").cloned().unwrap_or(Value::Null),
        "activeGroup":window.get("activeGroup").cloned().unwrap_or(Value::Null),
        "terminals":window.get("terminals").cloned().unwrap_or_else(|| json!([])),
        "activeTerminalId":window.get("activeTerminalId").cloned().unwrap_or(Value::Null),
    }))
}
fn first_document(windows: &[Value]) -> Option<Value> {
    windows.first().and_then(saved_document)
}
fn entry(
    profile: &str,
    id: &str,
    detached: Option<&str>,
    document: Value,
) -> RegistryWindowSnapshot {
    RegistryWindowSnapshot {
        id: id.into(),
        profile_id: profile.into(),
        detached_workspace_id: detached.map(str::to_owned),
        has_content: true,
        last_active_at: 1,
        workspace_document: document,
    }
}
fn read_request(profile: &str) -> ReadRequest {
    ReadRequest {
        profile_id: profile.into(),
    }
}
fn locate_request(id: &str) -> LocateRequest {
    LocateRequest {
        session_id: id.into(),
    }
}

#[test]
fn read_accepts_non_grammar_stored_ids_and_marks_eligibility() {
    let doc = document(
        json!([{"id":"workspace/with spaces"}]),
        json!([{"id":"terminal/with spaces","workspaceId":"workspace/with spaces"}]),
    );
    let entries = vec![entry("profile", "window/with spaces", None, doc.clone())];
    let result = execute_read(
        HOST,
        &read_request("profile"),
        &catalog("profile"),
        &entries,
        || panic!("live selection read disk"),
        first_document,
    )
    .unwrap();
    assert_eq!(result.document, doc);
    assert!(result.append_target_eligible);
    assert_eq!(result.window_id.as_deref(), Some("window/with spaces"));
    let mut duplicate = entries;
    duplicate[0].workspace_document["workspaces"] = json!([{"id":"x"},{"id":"x"}]);
    assert!(
        !execute_read(
            HOST,
            &read_request("profile"),
            &catalog("profile"),
            &duplicate,
            || panic!("live selection read disk"),
            first_document
        )
        .unwrap()
        .append_target_eligible
    );
    let empty = execute_read(
        HOST,
        &read_request("profile"),
        &catalog("profile"),
        &[],
        || Ok(SavedFileOutcome::NoSavedWindows),
        first_document,
    )
    .unwrap();
    assert_eq!(empty.document, empty_workspace_document());
    assert!(!empty.append_target_eligible);
}

#[test]
fn window_id_bound_is_inclusive_at_1024_json_bytes() {
    let doc = document(json!([{"id":"w"}]), json!([]));
    let good = entry("profile", &"w".repeat(1022), None, doc.clone());
    assert!(execute_read(
        HOST,
        &read_request("profile"),
        &catalog("profile"),
        &[good.clone()],
        || panic!("live"),
        first_document
    )
    .is_ok());
    assert!(execute_locate(
        HOST,
        &locate_request("t"),
        &catalog("profile"),
        &[good],
        &[],
        vec![],
        saved_document
    )
    .is_ok());
    for id in ["w".repeat(1023), "\u{1}".repeat(171)] {
        let bad = entry("profile", &id, Some("w"), doc.clone());
        assert!(matches!(
            execute_read(
                HOST,
                &read_request("profile"),
                &catalog("profile"),
                &[bad.clone()],
                || panic!("must not fall back"),
                first_document
            ),
            Err(OperationError::WorkspaceUnavailable)
        ));
        assert!(
            matches!(execute_locate(HOST, &locate_request("t"), &catalog("profile"), &[bad], &[], vec![], saved_document), Err(OperationError::LocationUnavailable(ids)) if ids == ["profile"])
        );
        let ignored = entry("remote", &id, None, doc.clone());
        assert!(execute_locate(
            HOST,
            &locate_request("t"),
            &catalog("profile"),
            &[ignored],
            &[],
            vec![],
            saved_document
        )
        .is_ok());
    }
}

#[test]
fn read_document_size_bound_and_largest_frame_b12() {
    let profile = "a".repeat(128);
    let mut doc = document(json!([{"padding":""}]), json!([]));
    let base = canonical_digest(&doc).unwrap().1;
    doc["workspaces"][0]["padding"] = json!("x".repeat(DOCUMENT_MAX_BYTES - base));
    let mut live = entry(&profile, &"w".repeat(1022), None, doc);
    let read = |live: &RegistryWindowSnapshot| {
        execute_read(
            HOST,
            &read_request(&profile),
            &catalog(&profile),
            std::slice::from_ref(live),
            || panic!("live"),
            first_document,
        )
    };
    let result = read(&live).unwrap();
    assert_eq!(
        canonical_digest(&result.document).unwrap().1,
        DOCUMENT_MAX_BYTES
    );
    let id = json!("\u{1}".repeat(128));
    let reply = json!({"ok":true,"value":result});
    let frame = json!({"type":"invoke-result","id":id,"result":reply});
    assert_eq!(serde_json::to_vec(&frame).unwrap().len(), 522_390);
    assert_eq!(crate::wire::bound_result(&id, reply.clone()), reply);
    live.workspace_document["workspaces"][0]["padding"] =
        json!("x".repeat(DOCUMENT_MAX_BYTES - base + 1));
    assert!(matches!(
        read(&live),
        Err(OperationError::WorkspaceTooLarge)
    ));
}

#[test]
fn locate_largest_frame_and_unavailable_frame_b12() {
    let observe = |workspace: &str| {
        let doc = document(
            json!([{"id":workspace}]),
            json!([{"id":"t","workspaceId":workspace}]),
        );
        execute_locate(
            HOST,
            &locate_request("t"),
            &catalog("p"),
            &[entry("p", "w", None, doc)],
            &[],
            vec![],
            saved_document,
        )
    };
    let initial = observe("").unwrap_err();
    assert!(matches!(initial, OperationError::LocationUnavailable(_)));
    let initial = observe("x").unwrap();
    let base = serde_json::to_vec(&initial).unwrap().len();
    let workspace = "x".repeat(DOCUMENT_MAX_BYTES - base + 1);
    let result = observe(&workspace).unwrap();
    assert_eq!(
        serde_json::to_vec(&result).unwrap().len(),
        DOCUMENT_MAX_BYTES
    );
    let id = json!("\u{1}".repeat(128));
    let reply = json!({"ok":true,"value":result});
    let frame = json!({"type":"invoke-result","id":id,"result":reply});
    assert_eq!(serde_json::to_vec(&frame).unwrap().len(), 521_022);
    assert_eq!(crate::wire::bound_result(&id, reply.clone()), reply);
    assert!(matches!(
        observe(&(workspace + "x")),
        Err(OperationError::LocationTooLarge)
    ));
    let ids = (0..256)
        .map(|i| format!("{i:03}{}", "a".repeat(125)))
        .collect();
    let reply = OperationError::LocationUnavailable(ids).reply();
    let frame = json!({"type":"invoke-result","id":id,"result":reply});
    assert_eq!(serde_json::to_vec(&frame).unwrap().len(), 34_489);
    assert_eq!(crate::wire::bound_result(&id, reply.clone()), reply);
}

#[test]
fn locate_detached_suppression_dedup_and_profile_revision() {
    let parent = document(
        json!([{"id":"a"},{"id":"b"}]),
        json!([{"id":"t","workspaceId":"a"},{"id":"t","workspaceId":"b"}]),
    );
    let selected = document(json!([{"id":"a"}]), json!([{"id":"t","workspaceId":"a"}]));
    let mut regular = entry("p", "regular", None, selected.clone());
    regular.last_active_at = 10;
    let detached = entry("p", "detached", Some("a"), parent.clone());
    let missing = entry("p", "missing", Some("missing"), parent.clone());
    let entries = vec![regular, detached.clone(), detached, missing];
    let saved = vec![SavedProfile {
        profile_id: "p".into(),
        outcome: SavedFileOutcome::Snapshot {
            raw_bytes: 1,
            windows: vec![parent.clone(), parent],
        },
    }];
    let locate = |entries: &[RegistryWindowSnapshot]| {
        execute_locate(
            HOST,
            &locate_request("t"),
            &catalog("p"),
            entries,
            &saved,
            vec![],
            saved_document,
        )
        .unwrap()
    };
    let result = locate(&entries);
    let positions: Vec<_> = result
        .locations
        .iter()
        .map(|row| {
            (
                row.workspace_id.as_str(),
                row.window_id.as_deref(),
                row.source,
            )
        })
        .collect();
    assert_eq!(
        positions,
        vec![
            ("a", Some("detached"), Source::Live),
            ("a", Some("regular"), Source::Live),
            ("b", None, Source::Persisted)
        ]
    );
    let revision = canonical_digest(&selected).unwrap().0;
    assert!(result
        .locations
        .iter()
        .all(|row| row.profile_revision == revision));
    let mut changed = entries;
    changed[3].workspace_document["terminals"][1]["title"] = json!("different unplaced content");
    assert_ne!(result.location_revision, locate(&changed).location_revision);
}

#[test]
fn locate_enforces_profile_terminal_match_and_result_bounds_in_order() {
    let many_profiles: Vec<_> = (0..257)
        .map(|i| CatalogEntry {
            id: format!("p{i}"),
            is_local: true,
        })
        .collect();
    assert!(matches!(
        execute_locate(
            HOST,
            &locate_request("t"),
            &many_profiles,
            &[],
            &[],
            vec![],
            saved_document
        ),
        Err(OperationError::LocationTooLarge)
    ));
    let terminals: Vec<_> = (0..4097)
        .map(|i| json!({"id":format!("t{i}"),"workspaceId":"w"}))
        .collect();
    let doc = document(json!([{"id":"w"}]), json!(terminals));
    assert!(matches!(
        execute_locate(
            HOST,
            &locate_request("not-present"),
            &catalog("p"),
            &[entry("p", "w", None, doc)],
            &[],
            vec![],
            saved_document
        ),
        Err(OperationError::LocationTooLarge)
    ));
    let doc = document(json!([{"id":"w"}]), json!([{"id":"t","workspaceId":"w"}]));
    let entries: Vec<_> = (0..101)
        .map(|i| entry("p", &format!("window-{i}"), None, doc.clone()))
        .collect();
    assert!(matches!(
        execute_locate(
            HOST,
            &locate_request("t"),
            &catalog("p"),
            &entries,
            &[],
            vec![],
            saved_document
        ),
        Err(OperationError::LocationTooLarge)
    ));
    let workspace = "x".repeat(6000);
    let doc = document(
        json!([{"id":workspace}]),
        json!([{"id":"t","workspaceId":workspace}]),
    );
    let entries: Vec<_> = (0..100)
        .map(|i| entry("p", &format!("window-{i}"), None, doc.clone()))
        .collect();
    assert!(matches!(
        execute_locate(
            HOST,
            &locate_request("t"),
            &catalog("p"),
            &entries,
            &[],
            vec![],
            saved_document
        ),
        Err(OperationError::LocationTooLarge)
    ));
    let malformed = entry("p", "window", None, document(json!([{"id":""}]), json!([])));
    assert!(
        matches!(execute_locate(HOST, &locate_request("t"), &catalog("p"), &[malformed], &[], vec!["z".into(),"p".into(),"z".into()], saved_document), Err(OperationError::LocationUnavailable(ids)) if ids == ["p", "z"])
    );
}

#[test]
fn saved_file_retry_reader_handles_torn_then_valid_and_8mib_limit() {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    let dir = std::env::temp_dir().join(format!(
        "bac-saved-reader-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("saved.json");
    assert!(matches!(
        read_bounded_saved_file(&path, Some),
        Ok(SavedFileOutcome::NoSavedWindows)
    ));
    fs::write(&path, b"{").unwrap();
    let replacement = path.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        fs::write(
            replacement,
            br#"{"version":2,"windows":[{"workspaces":[],"terminals":[]}]}"#,
        )
        .unwrap();
    });
    assert!(
        matches!(read_bounded_saved_file(&path, Some), Ok(SavedFileOutcome::Snapshot { windows, .. }) if windows.len() == 1)
    );
    writer.join().unwrap();
    let attempts = AtomicUsize::new(0);
    let result = read_bounded_saved_file(&path, |value| {
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            None
        } else {
            Some(value)
        }
    });
    assert!(matches!(result, Ok(SavedFileOutcome::Snapshot { .. })));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    fs::write(&path, b"{").unwrap();
    assert!(matches!(
        read_bounded_saved_file(&path, Some),
        Err(SavedReadError::Unreadable)
    ));
    fs::write(&path, br#"{"version":2,"windows":[]}"#).unwrap();
    let (result, count) = read_bounded_saved_file_measured(&path, Some);
    assert!(matches!(result, Ok(SavedFileOutcome::NoSavedWindows)));
    assert_eq!(count, fs::metadata(&path).unwrap().len() as usize);
    fs::File::create(&path)
        .unwrap()
        .set_len(SAVED_FILE_MAX_BYTES as u64 + 1)
        .unwrap();
    let invoked = AtomicUsize::new(0);
    let result = read_bounded_saved_file(&path, |value| {
        invoked.fetch_add(1, Ordering::SeqCst);
        Some(value)
    });
    assert!(matches!(result, Err(SavedReadError::TooLarge)));
    assert_eq!(invoked.load(Ordering::SeqCst), 0);
    fs::remove_dir_all(dir).unwrap();
}
