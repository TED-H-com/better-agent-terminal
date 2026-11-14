mod bac_tests {
    use super::*;
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, ClientConnection, RootCertStore};
    type Socket = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

    struct FakeSidecar {
        state: SidecarState,
        config: crate::sidecar::SpawnConfig,
    }
    impl FakeSidecar {
        fn call(&self, method: &str) -> Value {
            self.state
                .call_with_emit(
                    &self.config,
                    None,
                    method,
                    json!({}),
                    Duration::from_secs(5),
                )
                .unwrap()
        }
    }
    impl Drop for FakeSidecar {
        fn drop(&mut self) {
            let _ = self.state.call_with_emit(
                &self.config,
                None,
                "fixture.exit",
                json!({}),
                Duration::from_secs(5),
            );
        }
    }
    fn connect(host: &TestHost, v2: bool) -> (Socket, String, Value) {
        connect_id(host, v2, "auth")
    }
    fn connect_id(host: &TestHost, v2: bool, id: &str) -> (Socket, String, Value) {
        let cert = ensure_remote_certificate(&host.dir).unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(cert.cert_der)).unwrap();
        let config = Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let tcp =
            TcpStream::connect(("127.0.0.1", host.info["port"].as_u64().unwrap() as u16)).unwrap();
        assert_eq!(tcp.peer_addr().unwrap().ip(), std::net::Ipv4Addr::LOCALHOST);
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        let tls = StreamOwned::new(
            ClientConnection::new(config, ServerName::try_from("localhost").unwrap()).unwrap(),
            tcp,
        );
        let mut ws = tungstenite::client(format!("wss://localhost:{}/", host.info["port"]), tls)
            .unwrap()
            .0;
        let mut auth = json!({"type":"auth","id":id,"token":host.info["token"]});
        if v2 {
            auth["protocol"] = json!("bat-remote/v2");
        }
        ws.send(Message::Text(auth.to_string().into())).unwrap();
        loop {
            let text = ws.read().unwrap().into_text().unwrap().to_string();
            let value = decode_remote_text_frame(&text).unwrap();
            if value["type"] == "auth-result" || value["type"] == "invoke-error" {
                return (ws, text, value);
            }
        }
    }
    fn invoke(ws: &mut Socket, mut frame: Value) -> Value {
        frame["type"] = json!("invoke");
        frame["id"] = json!("request");
        ws.send(Message::Text(frame.to_string().into())).unwrap();
        loop {
            let value = decode_remote_text_frame(&ws.read().unwrap().into_text().unwrap()).unwrap();
            if value["id"] == "request" {
                assert_eq!(value["type"], "invoke-result", "{value}");
                return value["result"].clone();
            }
        }
    }
    fn hello(ws: &mut Socket) -> Value {
        invoke(
            ws,
            json!({"channel":"bac:hello","params":{"protocol":"bac/1","minor":0}}),
        )
    }
    fn error(reply: Value) -> String {
        reply["error"]["code"].as_str().unwrap().into()
    }
    fn names(host: &TestHost) -> Vec<String> {
        let mut result: Vec<_> = fs::read_dir(&host.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        result.sort();
        result
    }
    #[test]
    fn auth_bytes_only_add_constant_capability_and_v1_is_unchanged() {
        let baseline_child = std::env::var_os("BAC_TEST_BASELINE_AUTH").is_some();
        let measured_baseline: Vec<String> = if baseline_child {
            Vec::new()
        } else {
            let name = format!(
                "{}::auth_bytes_only_add_constant_capability_and_v1_is_unchanged",
                module_path!().split_once("::").unwrap().1
            );
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--test-threads=1", "--nocapture"])
                .env("BAC_TEST_BASELINE_AUTH", "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let frames: Vec<String> = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .filter_map(|line| {
                    line.split_once("BAC_BASELINE_FRAME ")
                        .map(|(_, raw)| raw.into())
                })
                .collect();
            assert_eq!(frames.len(), 2);
            frames
        };
        let host = TestHost::new();
        let before = names(&host);
        for (index, v2) in [true, false].into_iter().enumerate() {
            let (mut ws, raw, actual) = connect(&host, v2);
            let baseline = json!({"type":"auth-result","id":"auth","result":true,
                "protocol":if v2 {"bat-remote/v2"} else {"bat-remote/legacy-v1"},
                "compression":"none","serverVersion":host.ctx.version(),"capabilities":remote_capabilities()});
            let mut expected = baseline.clone();
            if v2 && !baseline_child {
                expected["capabilities"]["bac"] = json!({"protocol":"bac/1","minor":0});
            }
            assert_eq!(actual, expected);
            assert_eq!(raw, serde_json::to_string(&expected).unwrap());
            if baseline_child {
                println!("BAC_BASELINE_FRAME {raw}");
            } else if v2 {
                let member = "\"bac\":{\"minor\":0,\"protocol\":\"bac/1\"},";
                assert_eq!(raw.matches(member).count(), 1);
                assert_eq!(
                    raw.replacen(member, "", 1),
                    serde_json::to_string(&baseline).unwrap()
                );
                assert_eq!(raw.replacen(member, "", 1), measured_baseline[index]);
                let mut stripped = actual.clone();
                stripped["capabilities"]
                    .as_object_mut()
                    .unwrap()
                    .remove("bac");
                assert_eq!(
                    stripped,
                    serde_json::from_str::<Value>(&measured_baseline[index]).unwrap()
                );
            } else {
                assert_eq!(raw, serde_json::to_string(&baseline).unwrap());
                assert_eq!(raw, measured_baseline[index]);
            }
            ws.close(None).unwrap();
        }
        assert!(!host.dir.join("bac").exists());
        assert_eq!(names(&host), before);
    }
    #[test]
    fn wire_opt_in_is_per_connection_and_reconnect_and_never_falls_back() {
        // The upstream resolver reads a process-wide environment variable.
        // Isolate its fake-script override in a child test process, never in
        // the parent runner where unrelated tests may execute concurrently.
        if std::env::var_os("BAC_TEST_HANDSHAKE_CHILD").is_none() {
            let name = format!(
                "{}::wire_opt_in_is_per_connection_and_reconnect_and_never_falls_back",
                module_path!().split_once("::").unwrap().1
            );
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--test-threads=1"])
                .env("BAC_TEST_HANDSHAKE_CHILD", "1")
                .env(
                    "BAT_SIDECAR_SCRIPT",
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/src/bac/remote_server_fake_sidecar.mjs"
                    ),
                )
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child fixture failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let host = TestHost::new();
        // Pre-start only this fixture's fake. The production dispatcher keeps
        // its original resolver and uses the already-running fake bridge.
        let fake = host.dir.join("fake-sidecar.mjs");
        fs::write(&fake, include_str!("remote_server_fake_sidecar.mjs")).unwrap();
        let cfg = crate::sidecar::SpawnConfig {
            node_path: crate::sidecar::which_node().unwrap(),
            script_path: fake,
            data_dir: Some(host.dir.clone()),
            extra_env: vec![],
        };
        let fake = FakeSidecar {
            state: host.sidecar.clone(),
            config: cfg,
        };
        fake.call("fixture.reset");
        let (mut a, _, _) = connect(&host, true);
        let (mut b, _, _) = connect(&host, true);
        let channels = [
            "bac:workspace-read",
            "bac:workspace-add-terminal",
            "bac:session-locate",
            "bac:git-observe",
            "bac:future",
        ];
        for name in channels {
            for ws in [&mut a, &mut b] {
                assert_eq!(
                    error(invoke(ws, json!({"channel":name,"params":{}}))),
                    "NOT_OPTED_IN"
                );
            }
        }
        assert!(!host.dir.join("bac").exists());
        let first = hello(&mut a);
        assert_eq!(first["ok"], true);
        assert_eq!(hello(&mut a), first);
        assert_eq!(
            fs::read_dir(host.dir.join("bac"))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>(),
            vec![std::ffi::OsString::from("host.json")]
        );
        for name in channels {
            assert_eq!(
                error(invoke(&mut a, json!({"channel":name,"params":{}}))),
                if name == "bac:future" {
                    "UNKNOWN_CHANNEL"
                } else if name == "bac:workspace-read" || name == "bac:session-locate" {
                    "INVALID_REQUEST"
                } else {
                    "UNSUPPORTED_FEATURE"
                }
            );
            assert_eq!(
                error(invoke(&mut b, json!({"channel":name,"params":{}}))),
                "NOT_OPTED_IN"
            );
        }
        assert_eq!(fake.call("fixture.calls"), json!([]));
        let params = json!({"sessionId":"fixture-session","nested":{"kept":true},"text":"literal arguments"});
        let ordinary = invoke(
            &mut a,
            json!({"channel":"claude:export-transcript","params":params}),
        );
        assert_eq!(
            ordinary,
            json!({"method":"claude.exportTranscript","params":params})
        );
        assert_eq!(
            fake.call("fixture.calls"),
            json!([{"method":"claude.exportTranscript","params":params}])
        );
        a.close(None).unwrap();
        let (mut reconnect, _, _) = connect(&host, true);
        for name in channels {
            assert_eq!(
                error(invoke(&mut reconnect, json!({"channel":name,"params":{}}))),
                "NOT_OPTED_IN"
            );
        }
        assert_eq!(hello(&mut reconnect), first);
        b.close(None).unwrap();
        reconnect.close(None).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn ws_read_and_locate_empty_registry_persisted_roundtrip() {
        use std::{
            ffi::CString,
            io::Write,
            os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
            sync::mpsc,
            time::Instant,
        };
        let host = TestHost::new();
        let profiles = host.dir.join("profiles");
        let index = json!({"profiles":[
            {"id":"default","name":"Default","type":"local","createdAt":0,"updatedAt":0},
            {"id":"empty","name":"Empty","type":"local","createdAt":0,"updatedAt":0},
            {"id":"remote","name":"Remote","type":"remote","remoteToken":"fixture-only","createdAt":0,"updatedAt":0}
        ],"activeProfileIds":[]});
        let snapshot = json!({"version":2,"windows":[{"workspaces":[{"id":"w"}],
            "terminals":[{"id":"t","workspaceId":"w"}]}]});
        fs::write(
            profiles.join("index.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        fs::write(
            profiles.join("default.json"),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        fs::write(
            profiles.join("empty.json"),
            br#"{"version":2,"windows":[]}"#,
        )
        .unwrap();

        let (mut ws, _, _) = connect(&host, true);
        let first = hello(&mut ws);
        assert_eq!(first["ok"], true);
        assert_eq!(
            first["value"]["features"],
            json!({
                "workspaceRead":true,"workspaceAddTerminal":false,"sessionLocate":true,"gitObserve":false
            })
        );
        assert_eq!(
            first["value"]["limits"],
            json!({
                "requestBytes":65536,"resultBytes":524288,"registrationBytes":16384,
                "profiles":256,"terminals":4096,"locationMatches":100,"gitStatusBytes":4194304,
                "gitIndexBytes":16777216,"gitPaths":10000,"gitTimeoutMs":5000,"gitAttempts":2
            })
        );
        let read = invoke(
            &mut ws,
            json!({"channel":"bac:workspace-read","params":{"profileId":"default"}}),
        );
        assert_eq!(read["ok"], true);
        assert_eq!(read["value"]["source"], "persisted");
        assert!(read["value"]["windowId"].is_null());
        assert_eq!(read["value"]["document"]["terminals"][0]["id"], "t");
        let empty = invoke(
            &mut ws,
            json!({"channel":"bac:workspace-read","params":{"profileId":"empty"}}),
        );
        assert_eq!(
            empty["value"]["revision"],
            "sha256:66c1b7a04a05ee220e10341c9baec59bdf92885b193c2838d527a235d2ca5a9c"
        );
        assert_eq!(empty["value"]["appendTargetEligible"], false);
        let locate = invoke(
            &mut ws,
            json!({"channel":"bac:session-locate","params":{"sessionId":"t"}}),
        );
        assert_eq!(locate["value"]["disposition"], "located");
        assert_eq!(locate["value"]["consistency"], "sequential");
        assert_eq!(locate["value"]["atomic"], false);
        assert_eq!(locate["value"]["locations"].as_array().unwrap().len(), 1);
        assert_eq!(
            locate["value"]["locations"][0]["profileRevision"],
            read["value"]["revision"]
        );

        // A FIFO holds the bounded saved read inside dispatch while a second
        // invoke on this same connection must still complete its hello.
        let path = profiles.join("default.json");
        fs::remove_file(&path).unwrap();
        let fifo = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let writer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut file = loop {
                match fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(file) => break file,
                    Err(error)
                        if error.raw_os_error() == Some(libc::ENXIO)
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("FIFO reader did not start: {error}"),
                }
            };
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            file.write_all(&bytes).unwrap();
        });
        ws.send(Message::Text(
            json!({"type":"invoke","id":"slow-read",
            "channel":"bac:workspace-read","params":{"profileId":"default"}})
            .to_string()
            .into(),
        ))
        .unwrap();
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("saved read did not start");
        ws.send(Message::Text(
            json!({"type":"invoke","id":"during-read",
            "channel":"bac:hello","params":{"protocol":"bac/1","minor":0}})
            .to_string()
            .into(),
        ))
        .unwrap();
        loop {
            let value = decode_remote_text_frame(&ws.read().unwrap().into_text().unwrap()).unwrap();
            if value["id"] == "during-read" {
                assert_eq!(value["result"], first);
                break;
            }
            assert_ne!(
                value["id"], "slow-read",
                "slow read returned before release"
            );
        }
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        loop {
            let value = decode_remote_text_frame(&ws.read().unwrap().into_text().unwrap()).unwrap();
            if value["id"] == "slow-read" {
                assert_eq!(value["result"]["ok"], true);
                assert_eq!(
                    value["result"]["value"]["revision"],
                    read["value"]["revision"]
                );
                break;
            }
        }
        ws.close(None).unwrap();
    }
    #[test]
    fn wire_hello_validation_and_expected_identity() {
        fn snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Option<Vec<u8>>)> {
            fn visit(
                root: &std::path::Path,
                dir: &std::path::Path,
                out: &mut Vec<(std::path::PathBuf, Option<Vec<u8>>)>,
            ) {
                for entry in fs::read_dir(dir).unwrap() {
                    let path = entry.unwrap().path();
                    let relative = path.strip_prefix(root).unwrap().to_path_buf();
                    if path.is_dir() {
                        out.push((relative, None));
                        visit(root, &path, out);
                    } else {
                        out.push((relative, Some(fs::read(path).unwrap())));
                    }
                }
            }
            let mut result = Vec::new();
            visit(root, root, &mut result);
            result.sort();
            result
        }
        let host = TestHost::new();
        let before = snapshot(&host.dir);
        let (mut ws, _, _) = connect(&host, true);
        for (params, expected) in [
            (json!({"protocol":"other","minor":0}), "UNSUPPORTED_VERSION"),
            (json!({"protocol":"bac/1","minor":1}), "UNSUPPORTED_VERSION"),
            (
                json!({"protocol":"x".repeat(65536),"minor":0}),
                "PAYLOAD_TOO_LARGE",
            ),
        ] {
            assert_eq!(
                error(invoke(
                    &mut ws,
                    json!({"channel":"bac:hello","params":params})
                )),
                expected
            );
        }
        assert_eq!(
            error(invoke(
                &mut ws,
                json!({"channel":"bac:hello","contextId":null,"params":{"protocol":"bac/1","minor":0}})
            )),
            "CONTEXT_NOT_SUPPORTED"
        );
        assert!(!host.dir.join("bac").exists());
        assert_eq!(snapshot(&host.dir), before);
        let (mut legacy, _, _) = connect(&host, false);
        assert_eq!(error(hello(&mut legacy)), "UNSUPPORTED_VERSION");
        assert!(!host.dir.join("bac").exists());
        assert_eq!(snapshot(&host.dir), before);
        legacy.close(None).unwrap();
        assert_eq!(
            error(invoke(
                &mut ws,
                json!({"channel":"bac:hello","params":{"protocol":"bac/1","minor":0,"expectedHostId":"h_ffffffffffffffffffffffffffffffff"}})
            )),
            "HOST_MISMATCH"
        );
        assert_eq!(
            error(invoke(&mut ws, json!({"channel":"bac:future","params":{}}))),
            "NOT_OPTED_IN"
        );
        let first = hello(&mut ws);
        assert_eq!(
            invoke(
                &mut ws,
                json!({"channel":"bac:hello","params":{"protocol":"bac/1","minor":0,"expectedHostId":first["value"]["hostId"]}})
            ),
            first
        );
        assert_eq!(
            error(invoke(
                &mut ws,
                json!({"channel":"bac:future","contextId":"chained","params":{}})
            )),
            "CONTEXT_NOT_SUPPORTED"
        );
        ws.close(None).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn wire_storage_errors_keep_the_constant_advertisement() {
        use std::os::unix::fs::PermissionsExt;
        let host = TestHost::new();
        let dir = host.dir.join("bac");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("host.json");
        fs::write(&path, b"partial").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let (mut ws, _, auth) = connect(&host, true);
        assert_eq!(
            auth["capabilities"]["bac"],
            json!({"protocol":"bac/1","minor":0})
        );
        let reply = hello(&mut ws);
        assert_eq!(
            reply["error"]["details"],
            json!({"reason":"identity_contents"})
        );
        assert_eq!(error(reply), "STORAGE_CORRUPT");
        assert_eq!(fs::read(&path).unwrap(), b"partial");
        fs::remove_file(&path).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let reply = hello(&mut ws);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            reply["error"]["details"],
            json!({"reason":"directory_read_only"})
        );
        assert_eq!(error(reply), "STORAGE_UNAVAILABLE");
        assert_eq!(hello(&mut ws)["ok"], true);
        ws.close(None).unwrap();
    }
    #[test]
    fn wire_invalid_id_does_not_escape_result_limit() {
        let host = TestHost::new();
        let (mut ws, _, _) = connect(&host, true);
        assert_eq!(hello(&mut ws)["ok"], true);
        ws.send(Message::Text(
            json!({"type":"invoke", "id":"x".repeat(600000),
            "channel":"bac:hello","params":{"protocol":"bac/1","minor":0}})
            .to_string()
            .into(),
        ))
        .unwrap();
        loop {
            let text = ws.read().unwrap().into_text().unwrap();
            let frame = decode_remote_text_frame(&text).unwrap();
            if frame["type"] == "event" {
                continue;
            }
            assert!(text.len() < bac_host_contracts::wire::RESULT_BYTES);
            assert_eq!(frame["id"], Value::Null);
            assert_eq!(frame["result"]["error"]["code"], "INVALID_REQUEST");
            break;
        }
        ws.close(None).unwrap();
    }

    #[test]
    fn advertisement_crosses_fork_auth_size_cap_by_exactly_37_bytes() {
        // This cap is the fork's own; upstream has none for auth results. Remove
        // this test when the cap no longer applies to authentication results.
        let host = TestHost::new();
        let mut baseline = json!({"type":"auth-result","id":"","result":true,
            "protocol":"bat-remote/v2","compression":"none","serverVersion":host.ctx.version(),
            "capabilities":remote_capabilities()});
        let cap = crate::remote_core::REMOTE_RESPONSE_MAX_BYTES;
        let id = "x".repeat(cap - 1 - serde_json::to_vec(&baseline).unwrap().len());
        baseline["id"] = json!(id);
        assert_eq!(serde_json::to_vec(&baseline).unwrap().len(), cap - 1);
        let mut advertised = baseline.clone();
        advertised["capabilities"]["bac"] = bac_host_contracts::wire::advertisement();
        let member_bytes =
            serde_json::to_vec(&json!({"bac":bac_host_contracts::wire::advertisement()}))
                .unwrap()
                .len()
                - 1;
        assert_eq!(member_bytes, 37);
        assert_eq!(
            serde_json::to_vec(&advertised).unwrap().len(),
            cap - 1 + member_bytes
        );
        let (mut ws, _, reply) = connect_id(&host, true, &id);
        assert_eq!(reply["type"], "invoke-error");
        assert_eq!(reply["code"], "REMOTE_RESPONSE_TOO_LARGE");
        assert!(!host.dir.join("bac").exists());
        ws.close(None).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn storage_refusal_never_uses_data_directory_logger() {
        use std::os::unix::fs::symlink;
        use std::process::Command;
        if std::env::var_os("BAC_LOG_REGRESSION_CHILD").is_none() {
            let name = format!(
                "{}::storage_refusal_never_uses_data_directory_logger",
                module_path!().split_once("::").unwrap().1
            );
            for debug in [false, true] {
                let mut command = Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", &name, "--nocapture", "--test-threads=1"])
                    .env("BAC_LOG_REGRESSION_CHILD", "1");
                if debug {
                    command.env("BAT_DEBUG", "1");
                } else {
                    command.env_remove("BAT_DEBUG");
                }
                let output = command.output().unwrap();
                assert!(output.status.success(), "{output:?}");
                let stderr = String::from_utf8(output.stderr).unwrap();
                assert_eq!(stderr.matches("[bac.identity] refusal").count(), 1);
            }
            return;
        }
        let debug = std::env::var("BAT_DEBUG").as_deref() == Ok("1");
        for planted in [true, false] {
            let host = TestHost::new();
            let (mut ws, _, _) = connect(&host, true);
            let outside = host.dir.join("log-target");
            fs::create_dir(&outside).unwrap();
            symlink(&outside, host.dir.join("bac")).unwrap();
            if planted {
                if host.dir.join("logs").exists() {
                    fs::remove_dir_all(host.dir.join("logs")).unwrap();
                }
                symlink(&outside, host.dir.join("logs")).unwrap();
            }
            assert_eq!(error(hello(&mut ws)), "STORAGE_CORRUPT");
            assert_eq!(error(hello(&mut ws)), "STORAGE_CORRUPT");
            if debug {
                if let Ok(text) = fs::read_to_string(host.dir.join("logs/debug.log")) {
                    assert!(!text.contains("[bac.identity]"));
                }
            } else {
                assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
                assert!(!host.dir.join("debug.log").exists());
                if !planted {
                    assert!(!host.dir.join("logs").exists());
                }
            }
            ws.close(None).unwrap();
        }
    }
}
