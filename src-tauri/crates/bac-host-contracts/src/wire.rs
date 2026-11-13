use serde_json::{json, Value};
use std::sync::Mutex;

pub const REQUEST_BYTES: usize = 65_536;
pub const RESULT_BYTES: usize = 524_288;
pub const STAGE_A_CHANNELS: [(&str, &str); 4] = [
    ("bac:workspace-read", "workspaceRead"),
    ("bac:workspace-add-terminal", "workspaceAddTerminal"),
    ("bac:session-locate", "sessionLocate"),
    ("bac:git-observe", "gitObserve"),
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
            Self::Internal => ("INTERNAL_ERROR", "BAC request failed", json!({})),
        };
        json!({"ok":false,"error":{"code":code,"message":message,"retryable":false,"details":details}})
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
    json!({"protocol":"bac/1","minor":0,"hostId":host,"build":build(),
        "features":{"workspaceRead":false,"workspaceAddTerminal":false,"sessionLocate":false,"gitObserve":false},
        "limits":{"requestBytes":REQUEST_BYTES,"resultBytes":RESULT_BYTES,"registrationBytes":16384,
            "profiles":256,"terminals":4096,"locationMatches":100,"gitStatusBytes":4194304,
            "gitIndexBytes":16777216,"gitPaths":10000,"gitTimeoutMs":5000,"gitAttempts":2}})
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
    negotiated: Mutex<bool>,
}

impl Connection {
    /// The loader is called only by a shape-valid, context-free hello. Storage
    /// errors precede version negotiation as specified; no provider fallback.
    pub fn invoke(
        &self,
        v2: bool,
        channel: &str,
        frame: &Value,
        load_identity: impl FnOnce() -> Result<String, Error>,
    ) -> Value {
        let result = self
            .handle(v2, channel, frame, load_identity)
            .map(|value| json!({"ok":true,"value":value}))
            .unwrap_or_else(|err| err.reply());
        bound_result(&frame["id"], result)
    }

    fn handle(
        &self,
        v2: bool,
        channel: &str,
        frame: &Value,
        load_identity: impl FnOnce() -> Result<String, Error>,
    ) -> Result<Value, Error> {
        let mut negotiated = self.negotiated.lock().map_err(|_| Error::Internal)?;
        let hello = channel == "bac:hello";
        if !hello && !*negotiated {
            return Err(Error::NotOptedIn);
        }
        let object = frame.as_object().ok_or(Error::InvalidRequest("frame"))?;
        if frame["type"] != "invoke" {
            return Err(Error::InvalidRequest("type"));
        }
        if !frame["id"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 128 && !s.contains('\0'))
        {
            return Err(Error::InvalidRequest("id"));
        }
        if frame["channel"].as_str() != Some(channel)
            || channel.len() > 128
            || channel.contains('\0')
        {
            return Err(Error::InvalidRequest("channel"));
        }
        if object
            .keys()
            .any(|k| !["type", "id", "channel", "params", "contextId"].contains(&k.as_str()))
        {
            return Err(Error::InvalidRequest("frame"));
        }
        let params = frame["params"]
            .as_object()
            .ok_or(Error::InvalidRequest("params"))?;
        if hello {
            if params
                .keys()
                .any(|k| !["protocol", "minor", "expectedHostId"].contains(&k.as_str()))
            {
                return Err(Error::InvalidRequest("params"));
            }
            if !params
                .get("protocol")
                .is_some_and(|v| v.as_str().is_some_and(|s| !s.contains('\0')))
            {
                return Err(Error::InvalidRequest("params.protocol"));
            }
            if !params.get("minor").is_some_and(|v| v.as_u64().is_some()) {
                return Err(Error::InvalidRequest("params.minor"));
            }
            if let Some(expected) = params.get("expectedHostId") {
                if !expected.as_str().is_some_and(valid_host_id) {
                    return Err(Error::InvalidRequest("params.expectedHostId"));
                }
            }
        }
        if serde_json::to_vec(frame)
            .map_err(|_| Error::Internal)?
            .len()
            > REQUEST_BYTES
        {
            return Err(Error::PayloadTooLarge);
        }
        if object.contains_key("contextId") {
            return Err(Error::ContextNotSupported);
        }
        if hello {
            let host = load_identity()?;
            if !v2 || params["protocol"] != "bac/1" || params["minor"] != 0 {
                return Err(Error::UnsupportedVersion);
            }
            if params
                .get("expectedHostId")
                .is_some_and(|expected| expected != &host)
            {
                // An explicit identity mismatch revokes any earlier negotiation.
                *negotiated = false;
                return Err(Error::HostMismatch(host));
            }
            *negotiated = true;
            return Ok(hello_value(&host));
        }
        match STAGE_A_CHANNELS.iter().find(|(name, _)| *name == channel) {
            Some((_, feature)) => Err(Error::UnsupportedFeature(feature)),
            None => Err(Error::UnknownChannel),
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
