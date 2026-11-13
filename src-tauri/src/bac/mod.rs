//! BAC is intercepted before upstream profile routing or sidecar fallback.
use super::{ContextSink, HostContext, ProfileContexts, RemoteProtocol, SidecarState};
pub(super) use bac_host_contracts::wire::reply_id;
use bac_host_contracts::{
    identity::Identity,
    wire::{advertisement, Error},
};
#[derive(Default)]
pub(super) struct Connection(bac_host_contracts::wire::Connection);
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

pub(super) fn capabilities(mut original: Value, protocol: RemoteProtocol) -> Value {
    // Only the isolated baseline fixture process suppresses the hook. This
    // branch does not exist in production builds.
    #[cfg(test)]
    if std::env::var_os("BAC_TEST_BASELINE_AUTH").is_some() {
        return original;
    }
    if protocol == RemoteProtocol::V2 {
        original["bac"] = advertisement();
    }
    original
}

fn identity(ctx: &HostContext) -> Result<String, Error> {
    static HOSTS: OnceLock<Mutex<HashMap<PathBuf, Arc<Identity>>>> = OnceLock::new();
    let path = ctx.data_dir_opt().ok_or(Error::StorageUnavailable)?;
    // Keys come only from host configuration, never from BAC request parameters.
    // Separate fixture hosts in one process must not share an identity.
    let host = HOSTS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| Error::Internal)?
        .entry(path.clone())
        .or_default()
        .clone();
    host.get(&path)
}

impl Connection {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn invoke(
        &self,
        ctx: &HostContext,
        sidecar: &SidecarState,
        protocol: RemoteProtocol,
        channel: &str,
        frame: &Value,
        contexts: &ProfileContexts,
        sink: ContextSink,
    ) -> Result<Value, String> {
        if channel.starts_with("bac:") {
            return Ok(self
                .0
                .invoke(protocol == RemoteProtocol::V2, channel, frame, || {
                    identity(ctx)
                }));
        }
        super::invoke_with_profile_context(ctx, sidecar, protocol, channel, frame, contexts, sink)
    }
}
