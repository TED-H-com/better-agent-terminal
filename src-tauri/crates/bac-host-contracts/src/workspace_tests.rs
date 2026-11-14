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
        assert!(!valid_profile_id(bad));
        assert_eq!(
            parse_request("bac:workspace-read", &json!({"profileId":bad})),
            Err(OperationError::InvalidRequest("params.profileId"))
        );
    }
    assert!(valid_id("A-1_.:z"));
    assert!(valid_id("a:b"));
    assert_eq!(
        parse_request("bac:session-locate", &json!({"sessionId":"a:b"})),
        Ok(Request::SessionLocate(LocateRequest {
            session_id: "a:b".into(),
        }))
    );
    assert!(valid_profile_id("A-1_.z"));
    assert!(!valid_profile_id("a:b"));
    assert_eq!(
        parse_request("bac:workspace-read", &json!({"profileId":"a:b"})),
        Err(OperationError::InvalidRequest("params.profileId"))
    );
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
fn profile_id_grammar_allows_cjk_bounds_and_rejects_non_cjk_and_colon() {
    let chinese = "\u{4e2d}\u{6587}-_\u{4e00}\u{9fff}.1";
    assert!(valid_profile_id(chinese));
    assert!(!valid_id(chinese));
    assert_eq!(
        parse_request("bac:workspace-read", &json!({"profileId": chinese})),
        Ok(Request::WorkspaceRead(ReadRequest {
            profile_id: chinese.into(),
        }))
    );
    assert_eq!(
        parse_request("bac:session-locate", &json!({"sessionId": chinese})),
        Err(OperationError::InvalidRequest("params.sessionId"))
    );

    let cjk_42 = "\u{4e00}".repeat(42);
    assert_eq!(cjk_42.len(), 126);
    assert!(valid_profile_id(&cjk_42));
    assert!(parse_request("bac:workspace-read", &json!({"profileId": cjk_42})).is_ok());

    let cjk_128 = format!("{cjk_42}ab");
    assert_eq!(cjk_128.len(), 128);
    assert!(valid_profile_id(&cjk_128));

    let cjk_129 = format!("{cjk_42}abc");
    assert_eq!(cjk_129.len(), 129);
    assert!(!valid_profile_id(&cjk_129));

    let cjk_43 = "\u{4e00}".repeat(43);
    assert_eq!(cjk_43.len(), 129);
    assert!(!valid_profile_id(&cjk_43));
    assert_eq!(
        parse_request("bac:workspace-read", &json!({"profileId": cjk_43})),
        Err(OperationError::InvalidRequest("params.profileId"))
    );

    for rejected in ["\u{3042}", "\u{ac00}", "\u{4dff}", "\u{a000}", "a:b"] {
        assert!(!valid_profile_id(rejected));
        assert_eq!(
            parse_request("bac:workspace-read", &json!({"profileId": rejected})),
            Err(OperationError::InvalidRequest("params.profileId"))
        );
        let bad_catalog = catalog(rejected);
        assert_eq!(
            require_local_profile(&bad_catalog, rejected),
            Err(OperationError::ProfileNotFound)
        );
        assert!(matches!(
            execute_read(
                HOST,
                &read_request(rejected),
                &bad_catalog,
                &[],
                || panic!("invalid catalog must not read disk"),
                first_document
            ),
            Err(OperationError::ProfileNotFound)
        ));
        assert!(matches!(
            execute_locate(
                HOST,
                &locate_request("t"),
                &bad_catalog,
                &[],
                &[],
                vec![],
                saved_document
            ),
            Err(OperationError::LocationUnavailable(ids)) if ids.is_empty()
        ));
    }

    for accepted in [chinese, &cjk_42, ".", ".."] {
        assert!(valid_profile_id(accepted));
        let doc = document(json!([{"id":"w"}]), json!([{"id":"a:b","workspaceId":"w"}]));
        let entries = vec![entry(accepted, "w1", None, doc)];
        let read = execute_read(
            HOST,
            &read_request(accepted),
            &catalog(accepted),
            &entries,
            || panic!("live selection"),
            first_document,
        )
        .unwrap();
        assert_eq!(read.profile_id, accepted);
        let located = execute_locate(
            HOST,
            &locate_request("a:b"),
            &catalog(accepted),
            &entries,
            &[],
            vec![],
            saved_document,
        )
        .unwrap();
        assert_eq!(located.disposition, Disposition::Located);
        assert_eq!(located.locations[0].profile_id, accepted);
        assert_eq!(located.locations[0].terminal_id, "a:b");
    }
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

#[cfg(unix)]
fn run_with_fifo_deadline<T: Send + 'static>(
    fifo: &Path,
    action: impl FnOnce() -> T + Send + 'static,
) -> T {
    use std::{os::unix::fs::OpenOptionsExt, sync::mpsc};
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(action());
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(value) => {
            handle.join().unwrap();
            value
        }
        Err(error) => {
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(fifo);
            let _ = handle.join();
            panic!("FIFO operation did not return within 5s: {error}");
        }
    }
}

#[cfg(unix)]
#[test]
fn saved_file_rejects_non_regular_files_and_oversized_metadata_without_blocking() {
    use std::{
        ffi::CString,
        fs,
        os::unix::{ffi::OsStrExt, fs::symlink},
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    let dir = std::env::temp_dir().join(format!(
        "bac-saved-nonreg-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();

    let fifo_path = dir.join("fifo.json");
    let fifo = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    let fifo_for_read = fifo_path.clone();
    let ((result, count), migrate_calls) = run_with_fifo_deadline(&fifo_path, move || {
        let invoked = AtomicUsize::new(0);
        let outcome = read_bounded_saved_file_measured(&fifo_for_read, |value| {
            invoked.fetch_add(1, Ordering::SeqCst);
            Some(value)
        });
        (outcome, invoked.load(Ordering::SeqCst))
    });
    assert!(matches!(result, Err(SavedReadError::Unreadable)));
    assert_eq!(count, 0);
    assert_eq!(migrate_calls, 0);

    let fifo_for_open = fifo_path.clone();
    let open_result =
        run_with_fifo_deadline(&fifo_path, move || open_regular_saved_file(&fifo_for_open));
    assert!(matches!(open_result, Err(Err(SavedReadError::Unreadable))));

    for (name, target) in [("null.json", "/dev/null"), ("zero.json", "/dev/zero")] {
        let link = dir.join(name);
        symlink(target, &link).unwrap();
        let invoked = AtomicUsize::new(0);
        let (result, count) = read_bounded_saved_file_measured(&link, |value| {
            invoked.fetch_add(1, Ordering::SeqCst);
            Some(value)
        });
        assert!(matches!(result, Err(SavedReadError::Unreadable)));
        assert_eq!(count, 0);
        assert_eq!(invoked.load(Ordering::SeqCst), 0);
        assert!(matches!(
            open_regular_saved_file(&link),
            Err(Err(SavedReadError::Unreadable))
        ));
    }

    let subdir = dir.join("subdir.json");
    fs::create_dir(&subdir).unwrap();
    let (result, count) = read_bounded_saved_file_measured(&subdir, Some);
    assert!(matches!(result, Err(SavedReadError::Unreadable)));
    assert_eq!(count, 0);

    let sparse = dir.join("sparse.json");
    fs::File::create(&sparse)
        .unwrap()
        .set_len(SAVED_FILE_MAX_BYTES as u64 + 1)
        .unwrap();
    let invoked = AtomicUsize::new(0);
    let (result, count) = read_bounded_saved_file_measured(&sparse, |value| {
        invoked.fetch_add(1, Ordering::SeqCst);
        Some(value)
    });
    assert!(matches!(result, Err(SavedReadError::TooLarge)));
    assert_eq!(count, 0);
    assert_eq!(invoked.load(Ordering::SeqCst), 0);

    let dangling = dir.join("dangling.json");
    symlink(dir.join("missing.json"), &dangling).unwrap();
    let (result, count) = read_bounded_saved_file_measured(&dangling, Some);
    assert!(matches!(result, Ok(SavedFileOutcome::NoSavedWindows)));
    assert_eq!(count, 0);

    let regular = dir.join("regular.json");
    fs::write(
        &regular,
        br#"{"version":2,"windows":[{"workspaces":[],"terminals":[]}]}"#,
    )
    .unwrap();
    let regular_link = dir.join("regular-link.json");
    symlink(&regular, &regular_link).unwrap();
    let (result, count) = read_bounded_saved_file_measured(&regular_link, Some);
    assert!(matches!(result, Ok(SavedFileOutcome::Snapshot { .. })));
    assert_eq!(count, fs::metadata(&regular).unwrap().len() as usize);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reserved_profile_ids_respect_platform_and_skip_filesystem() {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    let mut reserved = vec![
        "CON", "con", "Con", "PRN", "prn", "AUX", "aux", "NUL", "nul", "con.foo", "CON.a.b",
        "NUL.", "prn.1", "aux.txt",
    ];
    let mut owned = Vec::new();
    for n in 0..=9 {
        owned.push(format!("COM{n}"));
        owned.push(format!("com{n}"));
        owned.push(format!("CoM{n}.ext"));
        owned.push(format!("LPT{n}"));
        owned.push(format!("lpt{n}"));
        owned.push(format!("LpT{n}.json"));
    }
    for s in &owned {
        reserved.push(s.as_str());
    }
    for id in &reserved {
        assert!(
            is_reserved_profile_id(id, true),
            "expected Windows reserved for {id}"
        );
        assert!(
            !is_reserved_profile_id(id, false),
            "expected non-reserved on non-Windows for {id}"
        );
    }
    for id in [
        "",
        ".",
        "..",
        ".con",
        "console",
        "null",
        "com",
        "com10",
        "lpt",
        "lpt10",
        "mycon",
        "con\u{4e00}",
        "\u{4e2d}\u{6587}",
    ] {
        assert!(
            !is_reserved_profile_id(id, true),
            "unexpected match for {id}"
        );
        assert!(
            !is_reserved_profile_id(id, false),
            "unexpected non-Windows match for {id}"
        );
    }

    let dir = std::env::temp_dir().join(format!(
        "bac-reserved-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let valid_snapshot = br#"{"version":2,"windows":[{"workspaces":[],"terminals":[]}]}"#;
    let con_path = dir.join("CON.json");
    let con_sub_path = dir.join("con.sub.json");
    let missing_nul = dir.join("NUL.json");
    fs::write(&con_path, valid_snapshot).unwrap();
    fs::write(&con_sub_path, valid_snapshot).unwrap();

    for path in [&con_path, &con_sub_path, &missing_nul] {
        let invoked = AtomicUsize::new(0);
        let (result, count) = read_bounded_saved_file_measured_for_platform(path, true, |value| {
            invoked.fetch_add(1, Ordering::SeqCst);
            Some(value)
        });
        assert!(matches!(result, Err(SavedReadError::Unreadable)));
        assert_eq!(count, 0);
        assert_eq!(invoked.load(Ordering::SeqCst), 0);
    }

    let (result, _) = read_bounded_saved_file_measured_for_platform(&con_path, false, Some);
    assert!(matches!(result, Ok(SavedFileOutcome::Snapshot { .. })));
    let (result, _) = read_bounded_saved_file_measured_for_platform(&con_sub_path, false, Some);
    assert!(matches!(result, Ok(SavedFileOutcome::Snapshot { .. })));
    let (result, count) = read_bounded_saved_file_measured_for_platform(&missing_nul, false, Some);
    assert!(matches!(result, Ok(SavedFileOutcome::NoSavedWindows)));
    assert_eq!(count, 0);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn duplicate_catalog_ids_first_entry_decides_for_read_and_locate() {
    let entry_local = |id: &str| CatalogEntry {
        id: id.into(),
        is_local: true,
    };
    let entry_remote = |id: &str| CatalogEntry {
        id: id.into(),
        is_local: false,
    };

    let dedup_catalog = vec![entry_local("dup"), entry_local("dup")];
    let resolved = local_catalog_profiles(&dedup_catalog).unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].id, "dup");

    let mut limit_with_duplicates: Vec<CatalogEntry> =
        (0..256).map(|i| entry_local(&format!("p{i}"))).collect();
    limit_with_duplicates.push(entry_local("p0"));
    assert_eq!(
        local_catalog_profiles(&limit_with_duplicates)
            .unwrap()
            .len(),
        256
    );
    limit_with_duplicates.push(entry_local("p256"));
    assert_eq!(
        local_catalog_profiles(&limit_with_duplicates).unwrap_err(),
        OperationError::LocationTooLarge
    );

    assert!(
        local_catalog_profiles(&[entry_remote("bad/id"), entry_local("bad/id")])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        local_catalog_profiles(&[entry_local("bad/id"), entry_remote("bad/id")]).unwrap_err(),
        OperationError::LocationUnavailable(vec![])
    );

    let live_doc = document(
        json!([{"id": "w"}]),
        json!([{"id": "t-dup", "workspaceId": "w"}]),
    );
    let live_entries = vec![entry("dup", "win-1", None, live_doc)];
    let once_read = execute_read(
        HOST,
        &read_request("dup"),
        &catalog("dup"),
        &live_entries,
        || panic!("live read should not touch disk"),
        first_document,
    )
    .unwrap();
    let twice_read = execute_read(
        HOST,
        &read_request("dup"),
        &dedup_catalog,
        &live_entries,
        || panic!("live read should not touch disk"),
        first_document,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&twice_read).unwrap(),
        serde_json::to_value(&once_read).unwrap()
    );

    let once_locate = execute_locate(
        HOST,
        &locate_request("t-dup"),
        &catalog("dup"),
        &live_entries,
        &[],
        vec![],
        saved_document,
    )
    .unwrap();
    let twice_locate = execute_locate(
        HOST,
        &locate_request("t-dup"),
        &dedup_catalog,
        &live_entries,
        &[],
        vec![],
        saved_document,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&twice_locate).unwrap(),
        serde_json::to_value(&once_locate).unwrap()
    );
    assert_eq!(
        twice_locate.location_revision,
        once_locate.location_revision
    );

    let remote_then_local = vec![entry_remote("dup"), entry_local("dup")];
    assert_eq!(
        execute_read(
            HOST,
            &read_request("dup"),
            &remote_then_local,
            &live_entries,
            || panic!("remote profile must not read disk"),
            first_document,
        )
        .unwrap_err(),
        OperationError::ProfileNotLocal
    );
    let remote_saved = vec![SavedProfile {
        profile_id: "dup".into(),
        outcome: SavedFileOutcome::Snapshot {
            raw_bytes: 16,
            windows: vec![json!({
                "workspaces": [{"id": "w"}],
                "terminals": [{"id": "t-dup", "workspaceId": "w"}]
            })],
        },
    }];
    let remote_locate = execute_locate(
        HOST,
        &locate_request("t-dup"),
        &remote_then_local,
        &live_entries,
        &remote_saved,
        vec![],
        saved_document,
    )
    .unwrap();
    let empty_locate = execute_locate(
        HOST,
        &locate_request("t-dup"),
        &[],
        &live_entries,
        &remote_saved,
        vec![],
        saved_document,
    )
    .unwrap();
    assert_eq!(remote_locate.disposition, Disposition::Unregistered);
    assert!(remote_locate.locations.is_empty());
    assert_eq!(
        remote_locate.location_revision,
        empty_locate.location_revision
    );

    let local_then_remote = vec![entry_local("dup"), entry_remote("dup")];
    let local_read = execute_read(
        HOST,
        &read_request("dup"),
        &local_then_remote,
        &live_entries,
        || panic!("live read should not touch disk"),
        first_document,
    )
    .unwrap();
    let local_locate = execute_locate(
        HOST,
        &locate_request("t-dup"),
        &local_then_remote,
        &live_entries,
        &[],
        vec![],
        saved_document,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&local_read).unwrap(),
        serde_json::to_value(&once_read).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&local_locate).unwrap(),
        serde_json::to_value(&once_locate).unwrap()
    );
}

#[test]
fn persisted_read_eligibility_requires_readable_raw_first_window() {
    let read_window = |window: Value| {
        execute_read(
            HOST,
            &read_request("p"),
            &catalog("p"),
            &[],
            || {
                Ok(SavedFileOutcome::Snapshot {
                    raw_bytes: 32,
                    windows: vec![window],
                })
            },
            first_document,
        )
        .unwrap()
    };

    for raw in [json!({}), Value::Null, json!(42)] {
        let result = read_window(raw);
        assert_eq!(result.source, Source::Persisted);
        assert_eq!(result.window_id, None);
        assert_eq!(result.document, empty_workspace_document());
        assert!(!result.append_target_eligible);
    }

    let missing_workspace_id = read_window(json!({"workspaces": [{}], "terminals": []}));
    assert_eq!(
        missing_workspace_id.document,
        document(json!([{}]), json!([]))
    );
    assert!(!missing_workspace_id.append_target_eligible);

    let valid_single = read_window(json!({
        "workspaces": [{"id": "w"}],
        "terminals": [{"id": "t", "workspaceId": "w"}]
    }));
    assert!(valid_single.append_target_eligible);
}
