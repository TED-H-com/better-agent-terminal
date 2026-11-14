use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier,
};
const HOST: &str = "h_0123456789abcdef0123456789abcdef";
fn frame(channel: &str) -> Value {
    json!({"type":"invoke","id":"i","channel":channel,"params":{}})
}
fn hello() -> Value {
    let mut f = frame("bac:hello");
    f["params"] = advertisement();
    f
}
fn call(c: &Connection, f: &Value) -> Value {
    c.invoke(
        true,
        f["channel"].as_str().unwrap_or(""),
        f,
        || Ok(HOST.into()),
        |_, _| panic!("unexpected feature dispatch"),
    )
}
fn code(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap()
}

#[test]
fn every_common_error_has_the_exact_envelope() {
    let cases = [
        (
            Error::NotOptedIn,
            "NOT_OPTED_IN",
            "Call bac:hello first",
            json!({}),
        ),
        (
            Error::InvalidRequest("id"),
            "INVALID_REQUEST",
            "Invalid BAC request",
            json!({"field":"id"}),
        ),
        (
            Error::PayloadTooLarge,
            "PAYLOAD_TOO_LARGE",
            "BAC request exceeds limit",
            json!({"maxBytes":65536}),
        ),
        (
            Error::ResponseTooLarge,
            "RESPONSE_TOO_LARGE",
            "BAC result exceeds limit",
            json!({"maxBytes":524288}),
        ),
        (
            Error::ContextNotSupported,
            "CONTEXT_NOT_SUPPORTED",
            "BAC requires a direct host connection",
            json!({}),
        ),
        (
            Error::UnsupportedVersion,
            "UNSUPPORTED_VERSION",
            "Unsupported BAC protocol version",
            advertisement(),
        ),
        (
            Error::HostMismatch(HOST.into()),
            "HOST_MISMATCH",
            "Host identity changed",
            json!({"hostId":HOST}),
        ),
        (
            Error::UnknownChannel,
            "UNKNOWN_CHANNEL",
            "Unknown BAC channel",
            json!({}),
        ),
        (
            Error::UnsupportedFeature("sessionLocate"),
            "UNSUPPORTED_FEATURE",
            "BAC feature is unavailable",
            json!({"feature":"sessionLocate"}),
        ),
        (
            Error::StorageUnavailable,
            "STORAGE_UNAVAILABLE",
            "BAC identity storage is unavailable",
            json!({}),
        ),
        (
            Error::StorageCorrupt,
            "STORAGE_CORRUPT",
            "BAC identity storage is invalid",
            json!({}),
        ),
        (
            Error::Internal,
            "INTERNAL_ERROR",
            "BAC request failed",
            json!({}),
        ),
    ];
    for (error, code, message, details) in cases {
        assert_eq!(
            error.reply(),
            json!({"ok":false,"error":{"code":code,"message":message,"retryable":false,"details":details}})
        );
    }
}
#[test]
fn opt_in_precedes_every_non_hello_validation_and_is_connection_local() {
    for name in STAGE_A_CHANNELS
        .iter()
        .map(|(n, _, _)| *n)
        .chain(["bac:unknown"])
    {
        let mut f = frame(name);
        f["contextId"] = Value::Null;
        f["params"] = json!("x".repeat(REQUEST_BYTES));
        assert_eq!(code(&call(&Connection::default(), &f)), "NOT_OPTED_IN");
    }
    let c = Connection::default();
    assert_eq!(call(&c, &hello())["ok"], true);
    assert_eq!(
        code(&call(&Connection::default(), &frame("bac:unknown"))),
        "NOT_OPTED_IN"
    );
}
#[test]
fn hello_is_idempotent_and_features_are_unavailable() {
    let c = Connection::default();
    let first = call(&c, &hello());
    assert_eq!(first["ok"], true);
    assert_eq!(first, call(&c, &hello()));
    assert_eq!(first["value"]["hostId"], HOST);
    for (name, feature, enabled) in STAGE_A_CHANNELS {
        assert_eq!(first["value"]["features"][feature], enabled);
        if !enabled {
            assert_eq!(
                call(&c, &frame(name)),
                Error::UnsupportedFeature(feature).reply()
            );
        }
    }
    assert_eq!(
        call(&c, &frame("bac:future")),
        Error::UnknownChannel.reply()
    );
}
#[test]
fn concurrent_hellos_and_following_requests_see_committed_negotiation() {
    let c = Arc::new(Connection::default());
    let barrier = Arc::new(Barrier::new(8));
    let jobs: Vec<_> = (0..8)
        .map(|_| {
            let c = c.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                let value = call(&c, &hello());
                assert_eq!(code(&call(&c, &frame("bac:future"))), "UNKNOWN_CHANNEL");
                value
            })
        })
        .collect();
    for job in jobs {
        assert_eq!(job.join().unwrap()["value"]["hostId"], HOST);
    }
}
#[test]
fn invalid_shapes_never_touch_storage() {
    let mut cases = vec![Value::Null];
    for (key, value) in [
        ("type", json!("event")),
        ("id", json!(4)),
        ("id", json!("")),
        ("id", json!("x".repeat(129))),
        ("channel", json!("bac:other")),
        ("params", Value::Null),
        ("args", json!([])),
    ] {
        let mut f = hello();
        f[key] = value;
        cases.push(f);
    }
    for (key, value) in [
        ("protocol", Value::Null),
        ("protocol", json!("bad\0")),
        ("minor", json!(-1)),
        ("minor", json!(0.5)),
        ("expectedHostId", Value::Null),
        ("expectedHostId", json!("bad")),
        ("extra", json!(1)),
    ] {
        let mut f = hello();
        f["params"][key] = value;
        cases.push(f);
    }
    for f in cases {
        let result = Connection::default().invoke(true, "bac:hello", &f, || {
            panic!("invalid shape read storage")
        }, |_, _| panic!("unexpected feature dispatch"));
        assert_eq!(code(&result), "INVALID_REQUEST", "{f}");
    }
}
#[test]
fn size_precedes_context_and_context_precedes_storage() {
    let c = Connection::default();
    let mut f = hello();
    f["contextId"] = Value::Null;
    assert_eq!(
        code(&c.invoke(true, "bac:hello", &f, || panic!("context touched storage"), |_, _| panic!("unexpected feature dispatch"))),
        "CONTEXT_NOT_SUPPORTED"
    );
    f["params"]["protocol"] = json!("x".repeat(REQUEST_BYTES));
    assert_eq!(
        code(&c.invoke(true, "bac:hello", &f, || panic!("oversize touched storage"), |_, _| panic!("unexpected feature dispatch"))),
        "PAYLOAD_TOO_LARGE"
    );
    f["params"]["minor"] = Value::Null;
    assert_eq!(code(&call(&c, &f)), "INVALID_REQUEST");
}
#[test]
fn version_precedes_storage_and_errors_do_not_opt_in() {
    let c = Connection::default();
    for (v2, protocol, minor) in [(true, "wrong", 0), (true, "bac/1", 1), (false, "bac/1", 0)] {
        let mut f = hello();
        f["params"] = json!({"protocol":protocol,"minor":minor});
        assert_eq!(
            c.invoke(v2, "bac:hello", &f, || panic!(
                "unsupported version must not touch storage"
            ), |_, _| panic!("unexpected feature dispatch")),
            Error::UnsupportedVersion.reply()
        );
        assert_eq!(code(&call(&c, &frame("bac:future"))), "NOT_OPTED_IN");
    }
    for err in [
        Error::StorageUnavailable,
        Error::StorageCorrupt,
        Error::Internal,
    ] {
        assert_eq!(
            c.invoke(true, "bac:hello", &hello(), || Err(err.clone()), |_, _| panic!("unexpected feature dispatch")),
            err.reply()
        );
        assert_eq!(code(&call(&c, &frame("bac:future"))), "NOT_OPTED_IN");
    }
}
#[test]
fn expected_identity_matches_and_mismatch_revokes_negotiation() {
    let c = Connection::default();
    let mut f = hello();
    f["params"]["expectedHostId"] = json!(HOST);
    assert_eq!(call(&c, &f)["ok"], true);
    f["params"]["expectedHostId"] = json!("h_ffffffffffffffffffffffffffffffff");
    assert_eq!(call(&c, &f), Error::HostMismatch(HOST.into()).reply());
    assert_eq!(code(&call(&c, &frame("bac:future"))), "NOT_OPTED_IN");
    assert_eq!(call(&c, &hello())["ok"], true);
}
#[test]
fn failed_version_hello_preserves_previous_negotiation() {
    let c = Connection::default();
    call(&c, &hello());
    let mut f = hello();
    f["params"]["minor"] = json!(1);
    assert_eq!(code(&call(&c, &f)), "UNSUPPORTED_VERSION");
    assert_eq!(code(&call(&c, &frame("bac:future"))), "UNKNOWN_CHANNEL");
}
#[test]
fn request_size_boundary_is_inclusive() {
    let c = Connection::default();
    call(&c, &hello());
    let mut f = frame("bac:future");
    f["params"]["padding"] = json!("");
    let extra = REQUEST_BYTES - serde_json::to_vec(&f).unwrap().len();
    f["params"]["padding"] = json!("x".repeat(extra));
    assert_eq!(code(&call(&c, &f)), "UNKNOWN_CHANNEL");
    f["params"]["padding"] = json!("x".repeat(extra + 1));
    assert_eq!(code(&call(&c, &f)), "PAYLOAD_TOO_LARGE");
}
#[test]
fn response_size_boundary_and_invalid_id_are_bounded() {
    let id = json!("i");
    let empty = json!({"type":"invoke-result","id":id,"result":""});
    let extra = RESULT_BYTES - serde_json::to_vec(&empty).unwrap().len();
    let value = json!("x".repeat(extra));
    assert_eq!(bound_result(&id, value.clone()), value);
    assert_eq!(
        bound_result(&id, json!("x".repeat(extra + 1))),
        Error::ResponseTooLarge.reply()
    );
    let mut f = hello();
    f["id"] = json!("x".repeat(RESULT_BYTES));
    assert_eq!(code(&call(&Connection::default(), &f)), "INVALID_REQUEST");
    assert_eq!(reply_id("bac:hello", f["id"].clone()), Value::Null);
    assert_eq!(reply_id("workspace:load", f["id"].clone()), f["id"]);
}
#[test]
fn non_hello_does_not_load_identity_again() {
    let c = Connection::default();
    call(&c, &hello());
    let n = AtomicUsize::new(0);
    c.invoke(true, "bac:future", &frame("bac:future"), || {
        n.fetch_add(1, Ordering::SeqCst);
        Ok(HOST.into())
    }, |_, _| panic!("unexpected feature dispatch"));
    assert_eq!(n.load(Ordering::SeqCst), 0);
}
#[test]
fn poisoned_connection_returns_internal_error() {
    let c = Connection::default();
    let _ = std::panic::catch_unwind(|| {
        let _g = c.negotiated.lock().unwrap();
        panic!("injected poison");
    });
    assert_eq!(call(&c, &hello()), Error::Internal.reply());
}

#[test]
fn hello_features_and_limits_match_dispatch_constants() {
    let value = hello_value(HOST);
    assert_eq!(
        value["features"].as_object().unwrap().len(),
        STAGE_A_CHANNELS.len()
    );
    for (_, feature, enabled) in STAGE_A_CHANNELS {
        assert_eq!(value["features"][feature], enabled);
    }
    assert_eq!(
        value["limits"],
        json!({
            "requestBytes":REQUEST_BYTES,"resultBytes":RESULT_BYTES,
            "registrationBytes":REGISTRATION_BYTES,"profiles":PROFILES_LIMIT,
            "terminals":TERMINALS_LIMIT,"locationMatches":LOCATION_MATCHES_LIMIT,
            "gitStatusBytes":GIT_STATUS_BYTES_LIMIT,"gitIndexBytes":GIT_INDEX_BYTES_LIMIT,
            "gitPaths":GIT_PATHS_LIMIT,"gitTimeoutMs":GIT_TIMEOUT_MS_LIMIT,
            "gitAttempts":GIT_ATTEMPTS_LIMIT
        })
    );
}

#[test]
fn dispatch_releases_negotiation_guard_before_run() {
    use std::{sync::mpsc, time::Duration};
    let connection = Arc::new(Connection::default());
    assert_eq!(call(&connection, &hello())["ok"], true);
    let mut request = frame("bac:workspace-read");
    request["params"] = json!({"profileId":"default"});
    let inside = connection.clone();
    let result = connection.invoke(
        true,
        "bac:workspace-read",
        &request,
        || panic!("feature must not load identity"),
        |_, _| {
            let (send, receive) = mpsc::channel();
            let other = std::thread::spawn(move || {
                send.send(call(&inside, &hello())).unwrap();
            });
            let concurrent = receive
                .recv_timeout(Duration::from_secs(5))
                .expect("hello blocked while feature ran");
            assert_eq!(concurrent["ok"], true);
            other.join().unwrap();
            Ok(json!({"complete":true}))
        },
    );
    assert_eq!(result["value"]["complete"], true);
}

#[test]
fn dispatch_disabled_feature_never_calls_run() {
    let connection = Connection::default();
    assert_eq!(call(&connection, &hello())["ok"], true);
    for (name, feature, enabled) in STAGE_A_CHANNELS {
        if !enabled {
            let reply = connection.invoke(
                true,
                name,
                &frame(name),
                || panic!("feature must not load identity"),
                |_, _| panic!("disabled feature ran"),
            );
            assert_eq!(reply, Error::UnsupportedFeature(feature).reply());
        }
    }
}

#[test]
fn dispatch_invalid_params_never_calls_run() {
    let connection = Connection::default();
    assert_eq!(call(&connection, &hello())["ok"], true);
    for (name, key, field) in [
        ("bac:workspace-read", "profileId", "params.profileId"),
        ("bac:session-locate", "sessionId", "params.sessionId"),
    ] {
        for (params, invalid_field) in [
            (Value::Null, "params"),
            (json!({}), field),
            (json!({(key): null}), field),
            (json!({(key): ""}), field),
            (json!({(key): "bad/id"}), field),
            (json!({(key): "x".repeat(129)}), field),
            (json!({(key): "valid", "extra": true}), "params"),
        ] {
            let mut request = frame(name);
            request["params"] = params;
            let reply = connection.invoke(
                true,
                name,
                &request,
                || panic!("feature must not load identity"),
                |_, _| panic!("invalid request ran"),
            );
            assert_eq!(
                reply,
                crate::workspace::OperationError::InvalidRequest(invalid_field).reply()
            );
        }
    }
}

#[test]
fn dispatch_passes_negotiated_host_id_and_revokes_on_mismatch() {
    let connection = Connection::default();
    assert_eq!(call(&connection, &hello())["ok"], true);
    for (channel, params) in [
        ("bac:workspace-read", json!({"profileId":"default"})),
        ("bac:session-locate", json!({"sessionId":"terminal"})),
    ] {
        let mut request = frame(channel);
        request["params"] = params;
        let reply = connection.invoke(
            true,
            channel,
            &request,
            || panic!("feature must not load identity"),
            |host, _| {
                assert_eq!(host, HOST);
                Ok(json!({"host":host}))
            },
        );
        assert_eq!(reply["value"]["host"], HOST);
    }
    let mut bad_hello = hello();
    bad_hello["params"]["expectedHostId"] = json!("h_ffffffffffffffffffffffffffffffff");
    assert_eq!(code(&call(&connection, &bad_hello)), "HOST_MISMATCH");
    let mut request = frame("bac:workspace-read");
    request["params"] = json!({"profileId":"default"});
    let reply = connection.invoke(
        true,
        "bac:workspace-read",
        &request,
        || panic!("revoked feature loaded identity"),
        |_, _| panic!("revoked feature ran"),
    );
    assert_eq!(code(&reply), "NOT_OPTED_IN");
}
