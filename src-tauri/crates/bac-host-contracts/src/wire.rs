use serde_json::{json, Value};
use std::sync::Mutex;

pub const REQUEST_BYTES: usize = 65_536;
pub const RESULT_BYTES: usize = 524_288;
pub const REGISTRATION_BYTES: usize = 16_384;
pub const PROFILES_LIMIT: usize = 256;
pub const TERMINALS_LIMIT: usize = 4_096;
pub const LOCATION_MATCHES_LIMIT: usize = 100;
pub const GIT_STATUS_BYTES_LIMIT: usize = 4_194_304;
pub const GIT_INDEX_BYTES_LIMIT: usize = 16_777_216;
pub const GIT_PATHS_LIMIT: usize = 10_000;
pub const GIT_TIMEOUT_MS_LIMIT: usize = 5_000;
pub const GIT_ATTEMPTS_LIMIT: usize = 2;
pub const WORKSPACE_READ_ENABLED: bool = true;
pub const WORKSPACE_ADD_TERMINAL_ENABLED: bool = false;
pub const SESSION_LOCATE_ENABLED: bool = true;
pub const GIT_OBSERVE_ENABLED: bool = false;
pub const STAGE_A_CHANNELS: [(&str, &str, bool); 4] = [
    (
        "bac:workspace-read",
        "workspaceRead",
        WORKSPACE_READ_ENABLED,
    ),
    (
        "bac:workspace-add-terminal",
        "workspaceAddTerminal",
        WORKSPACE_ADD_TERMINAL_ENABLED,
    ),
    (
        "bac:session-locate",
        "sessionLocate",
        SESSION_LOCATE_ENABLED,
    ),
    ("bac:git-observe", "gitObserve", GIT_OBSERVE_ENABLED),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotOptedIn,
    InvalidRequest(&'static str),
    PayloadTooLarge,
    ResponseTooLarge,
    ContextNotSupported,
    UnsupportedVersion,
    HostMismatch(String),
    UnknownChannel,
    UnsupportedFeature(&'static str),
    StorageUnavailable,
    StorageCorrupt,
    Internal,
    StorageReason(Box<Error>, &'static str),
}

pub(crate) fn error_reply(
    code: &'static str,
    message: &'static str,
    retryable: bool,
    details: Value,
) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message,"retryable":retryable,"details":details}})
}

impl Error {
    pub fn reply(&self) -> Value {
        let (code, message, details) = match self {
            Self::NotOptedIn => ("NOT_OPTED_IN", "Call bac:hello first", json!({})),
            Self::InvalidRequest(field) => (
                "INVALID_REQUEST",
                "Invalid BAC request",
                json!({"field":field}),
            ),
            Self::PayloadTooLarge => (
                "PAYLOAD_TOO_LARGE",
                "BAC request exceeds limit",
                json!({"maxBytes":REQUEST_BYTES}),
            ),
            Self::ResponseTooLarge => (
                "RESPONSE_TOO_LARGE",
                "BAC result exceeds limit",
                json!({"maxBytes":RESULT_BYTES}),
            ),
            Self::ContextNotSupported => (
                "CONTEXT_NOT_SUPPORTED",
                "BAC requires a direct host connection",
                json!({}),
            ),
            Self::UnsupportedVersion => (
                "UNSUPPORTED_VERSION",
                "Unsupported BAC protocol version",
                advertisement(),
            ),
            Self::HostMismatch(host) => (
                "HOST_MISMATCH",
                "Host identity changed",
                json!({"hostId":host}),
            ),
            Self::UnknownChannel => ("UNKNOWN_CHANNEL", "Unknown BAC channel", json!({})),
            Self::UnsupportedFeature(feature) => (
                "UNSUPPORTED_FEATURE",
                "BAC feature is unavailable",
                json!({"feature":feature}),
            ),
            Self::StorageUnavailable => (
                "STORAGE_UNAVAILABLE",
                "BAC identity storage is unavailable",
                json!({}),
            ),
            Self::StorageCorrupt => (
                "STORAGE_CORRUPT",
                "BAC identity storage is invalid",
                json!({}),
            ),
            Self::StorageReason(error, reason) => {
                let mut reply = error.reply();
                reply["error"]["details"] = json!({"reason":reason});
                return reply;
            }
            Self::Internal => ("INTERNAL_ERROR", "BAC request failed", json!({})),
        };
        error_reply(code, message, false, details)
    }
}

pub fn advertisement() -> Value {
    json!({"protocol":"bac/1","minor":0})
}

fn build() -> Value {
    let revision = option_env!("BAC_BUILD_REVISION")
        .filter(|s| {
            s.len() == 40
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .unwrap_or("unknown");
    let release = option_env!("BAC_BUILD_RELEASE")
        .filter(|s| !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| (0x20..=0x7e).contains(&b)))
        .unwrap_or("dev");
    let dirty = match option_env!("BAC_BUILD_DIRTY") {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    json!({"product":"bac","revision":revision,"release":release,"dirty":dirty})
}

pub fn hello_value(host: &str) -> Value {
    let features: serde_json::Map<String, Value> = STAGE_A_CHANNELS
        .iter()
        .map(|(_, feature, enabled)| ((*feature).to_owned(), Value::Bool(*enabled)))
        .collect();
    json!({"protocol":"bac/1","minor":0,"hostId":host,"build":build(),
        "features":features,
        "limits":{"requestBytes":REQUEST_BYTES,"resultBytes":RESULT_BYTES,"registrationBytes":REGISTRATION_BYTES,
            "profiles":PROFILES_LIMIT,"terminals":TERMINALS_LIMIT,"locationMatches":LOCATION_MATCHES_LIMIT,
            "gitStatusBytes":GIT_STATUS_BYTES_LIMIT,"gitIndexBytes":GIT_INDEX_BYTES_LIMIT,
            "gitPaths":GIT_PATHS_LIMIT,"gitTimeoutMs":GIT_TIMEOUT_MS_LIMIT,"gitAttempts":GIT_ATTEMPTS_LIMIT}})
}

pub fn valid_host_id(s: &str) -> bool {
    s.len() == 34
        && s.starts_with("h_")
        && s[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One per websocket. No I/O or host identity is acquired by construction.
#[derive(Default)]
pub struct Connection {
    negotiated: Mutex<Option<String>>,
}

enum Prepared {
    Hello(Value),
    Feature {
        host_id: String,
        request: crate::workspace::Request,
    },
}

impl Connection {
    /// Feature work starts only after preparation has released the negotiation lock.
    pub fn invoke(
        &self,
        v2: bool,
        channel: &str,
        frame: &Value,
        load_identity: impl FnOnce() -> Result<String, Error>,
        run: impl FnOnce(
            &str,
            crate::workspace::Request,
        ) -> Result<Value, crate::workspace::OperationError>,
    ) -> Value {
        let result = match self.prepare(v2, channel, frame, load_identity) {
            Ok(Prepared::Hello(value)) => json!({"ok":true,"value":value}),
            Ok(Prepared::Feature { host_id, request }) => match run(&host_id, request) {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(error) => error.reply(),
            },
            Err(reply) => reply,
        };
        bound_result(frame.get("id").unwrap_or(&Value::Null), result)
    }

    fn prepare(
        &self,
        v2: bool,
        channel: &str,
        frame: &Value,
        load_identity: impl FnOnce() -> Result<String, Error>,
    ) -> Result<Prepared, Value> {
        let mut negotiated = self
            .negotiated
            .lock()
            .map_err(|_| Error::Internal.reply())?;
        let hello = channel == "bac:hello";
        if !hello && negotiated.is_none() {
            return Err(Error::NotOptedIn.reply());
        }
        let object = frame
            .as_object()
            .ok_or_else(|| Error::InvalidRequest("frame").reply())?;
        if object.get("type").and_then(Value::as_str) != Some("invoke") {
            return Err(Error::InvalidRequest("type").reply());
        }
        if !object
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && s.len() <= 128 && !s.contains('\0'))
        {
            return Err(Error::InvalidRequest("id").reply());
        }
        if object.get("channel").and_then(Value::as_str) != Some(channel)
            || channel.len() > 128
            || channel.contains('\0')
        {
            return Err(Error::InvalidRequest("channel").reply());
        }
        if object
            .keys()
            .any(|k| !["type", "id", "channel", "params", "contextId"].contains(&k.as_str()))
        {
            return Err(Error::InvalidRequest("frame").reply());
        }
        let params_value = object
            .get("params")
            .ok_or_else(|| Error::InvalidRequest("params").reply())?;
        let params = params_value
            .as_object()
            .ok_or_else(|| Error::InvalidRequest("params").reply())?;
        if hello {
            if params
                .keys()
                .any(|k| !["protocol", "minor", "expectedHostId"].contains(&k.as_str()))
            {
                return Err(Error::InvalidRequest("params").reply());
            }
            if !params
                .get("protocol")
                .is_some_and(|v| v.as_str().is_some_and(|s| !s.contains('\0')))
            {
                return Err(Error::InvalidRequest("params.protocol").reply());
            }
            if !params.get("minor").is_some_and(|v| v.as_u64().is_some()) {
                return Err(Error::InvalidRequest("params.minor").reply());
            }
            if let Some(expected) = params.get("expectedHostId") {
                if !expected.as_str().is_some_and(valid_host_id) {
                    return Err(Error::InvalidRequest("params.expectedHostId").reply());
                }
            }
        }
        if serde_json::to_vec(frame)
            .map_err(|_| Error::Internal.reply())?
            .len()
            > REQUEST_BYTES
        {
            return Err(Error::PayloadTooLarge.reply());
        }
        if object.contains_key("contextId") {
            return Err(Error::ContextNotSupported.reply());
        }
        if hello {
            if !v2
                || params.get("protocol").and_then(Value::as_str) != Some("bac/1")
                || params.get("minor").and_then(Value::as_u64) != Some(0)
            {
                return Err(Error::UnsupportedVersion.reply());
            }
            let host = load_identity().map_err(|error| error.reply())?;
            if params
                .get("expectedHostId")
                .is_some_and(|expected| expected != &host)
            {
                *negotiated = None;
                return Err(Error::HostMismatch(host).reply());
            }
            *negotiated = Some(host.clone());
            return Ok(Prepared::Hello(hello_value(&host)));
        }
        match STAGE_A_CHANNELS
            .iter()
            .find(|(name, _, _)| *name == channel)
        {
            None => Err(Error::UnknownChannel.reply()),
            Some((_, feature, false)) => Err(Error::UnsupportedFeature(feature).reply()),
            Some((_, _, true)) => {
                let request = crate::workspace::parse_request(channel, params_value)
                    .map_err(|error| error.reply())?;
                let host_id = negotiated
                    .clone()
                    .ok_or_else(|| Error::NotOptedIn.reply())?;
                Ok(Prepared::Feature { host_id, request })
            }
        }
    }
}

/// Never reflect an invalid/unbounded BAC invoke ID into the response envelope.
/// Ordinary upstream channels retain their original ID verbatim.
pub fn reply_id(channel: &str, id: Value) -> Value {
    if channel.starts_with("bac:")
        && !id
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 128 && !s.contains('\0'))
    {
        Value::Null
    } else {
        id
    }
}

pub fn bound_result(id: &Value, result: Value) -> Value {
    let frame = json!({"type":"invoke-result","id":reply_id("bac:", id.clone()),"result":result});
    if serde_json::to_vec(&frame).map_or(true, |bytes| bytes.len() > RESULT_BYTES) {
        Error::ResponseTooLarge.reply()
    } else {
        result
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
