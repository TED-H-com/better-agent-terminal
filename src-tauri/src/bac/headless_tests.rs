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
        let mut auth = json!({"type":"auth","id":"auth","token":host.info["token"]});
        if v2 {
            auth["protocol"] = json!("bat-remote/v2");
        }
        ws.send(Message::Text(auth.to_string().into())).unwrap();
        loop {
            let text = ws.read().unwrap().into_text().unwrap().to_string();
            let value = decode_remote_text_frame(&text).unwrap();
            if value["type"] == "auth-result" {
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
    #[cfg(unix)]
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
                    concat!(env!("CARGO_MANIFEST_DIR"), "/src/bac/fake-sidecar.mjs"),
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
        fs::write(&fake, include_str!("fake-sidecar.mjs")).unwrap();
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
    fn wire_hello_validation_and_expected_identity() {
        let host = TestHost::new();
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
        assert_eq!(error(hello(&mut ws)), "STORAGE_CORRUPT");
        assert_eq!(fs::read(&path).unwrap(), b"partial");
        fs::remove_file(&path).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let reply = hello(&mut ws);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(error(reply), "STORAGE_UNAVAILABLE");
        assert_eq!(hello(&mut ws)["ok"], true);
        ws.close(None).unwrap();
    }
    #[cfg(unix)]
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
    #[cfg(not(unix))]
    #[test]
    fn wire_identity_fails_closed_without_a_reviewed_permission_backend() {
        let host = TestHost::new();
        let (mut ws, _, auth) = connect(&host, true);
        assert_eq!(
            auth["capabilities"]["bac"],
            json!({"protocol":"bac/1","minor":0})
        );
        assert_eq!(error(hello(&mut ws)), "STORAGE_UNAVAILABLE");
        assert!(!host.dir.join("bac").exists());
        ws.close(None).unwrap();
    }
}
