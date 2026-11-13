//! Host-owned, opt-in continuation of turns interrupted by subscription limits.
//! Only local runtime events are observed; proxied remote events never run here.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::host_context::HostContext;

const RESET_GRACE_MS: u64 = 2_000;

#[derive(Default)]
struct LimitedTurn {
    provider: &'static str,
    rejected: bool,
    ended: bool,
    resets: HashMap<String, u64>,
}

struct PendingResume {
    at: u64,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct ResumeState {
    turns: HashMap<String, LimitedTurn>,
    pending: HashMap<String, PendingResume>,
}

impl ResumeState {
    fn cancel(&mut self, id: &str) {
        self.turns.remove(id);
        if let Some(pending) = self.pending.remove(id) {
            pending.cancelled.store(true, Ordering::SeqCst);
        }
    }

    fn clear(&mut self) {
        for pending in self.pending.values() {
            pending.cancelled.store(true, Ordering::SeqCst);
        }
        self.pending.clear();
        self.turns.clear();
    }

    fn observe(&mut self, topic: &str, payload: &Value, provider: &'static str) {
        let Some(id) = payload["sessionId"].as_str() else {
            return;
        };
        if topic == "claude:session-reset"
            || (topic == "claude:message" && payload["message"]["role"] == "user")
        {
            self.cancel(id);
            return;
        }
        match topic {
            "claude:rate-limit" => {
                let info = &payload["info"];
                let turn = self.turns.entry(id.into()).or_default();
                turn.provider = provider;
                if let Some(kind) = info["rateLimitType"].as_str() {
                    if info["status"] == "rejected"
                        || info["utilization"].as_f64().is_some_and(|u| u >= 1.0)
                    {
                        if let Some(reset) = reset_millis(&info["resetsAt"]) {
                            turn.resets.insert(kind.into(), reset);
                        }
                    } else {
                        turn.resets.remove(kind);
                    }
                }
                if info["status"] == "rejected" && info["isUsingOverage"] != true {
                    turn.rejected = true;
                }
            }
            "claude:message" => {
                let message = &payload["message"];
                if message["role"] != "assistant" || !message["parentToolUseId"].is_null() {
                    return;
                }
                if message["error"] == "rate_limit" {
                    let turn = self.turns.entry(id.into()).or_default();
                    turn.provider = provider;
                    turn.rejected = true;
                } else if let Some(turn) = self.turns.get_mut(id) {
                    // A successful answer after an SDK retry means the turn recovered.
                    turn.rejected = false;
                    if turn.ended {
                        self.cancel(id);
                    }
                }
            }
            "claude:error" => {
                if payload["error"].as_str().is_some_and(is_usage_limit_error) {
                    let turn = self.turns.entry(id.into()).or_default();
                    turn.provider = provider;
                    turn.rejected = true;
                }
            }
            "claude:tool-use" => {
                if payload["toolCall"]["parentToolUseId"].is_null() {
                    if let Some(turn) = self.turns.get_mut(id) {
                        turn.rejected = false;
                    }
                }
            }
            "claude:turn-end" => {
                let end = &payload["payload"];
                if !matches!(end["reason"].as_str(), Some("completed" | "error")) {
                    self.cancel(id);
                    return;
                }
                if end["error"].as_str().is_some_and(is_usage_limit_error) {
                    let turn = self.turns.entry(id.into()).or_default();
                    turn.provider = provider;
                    turn.rejected = true;
                }
                if let Some(turn) = self.turns.get_mut(id) {
                    if turn.rejected {
                        turn.ended = true;
                    } else {
                        self.cancel(id);
                    }
                }
            }
            _ => {}
        }
    }

    fn deadline(&self, id: &str, snapshots: &Value, now: u64) -> Option<u64> {
        let turn = self.turns.get(id)?;
        if !turn.rejected || !turn.ended {
            return None;
        }
        let snapshot = &snapshots[turn.provider];
        let snapshot_resets = ["fiveHour", "sevenDay"].into_iter().filter_map(|key| {
            let window = &snapshot[key];
            if !snapshot["stale"].is_null() || window["utilization"].as_f64()? < 1.0 {
                return None;
            }
            reset_millis(&window["resetsAt"])
        });
        // Both the five-hour AND weekly cap may be exhausted. Wait for all
        // known blocking windows, rather than waking at the earlier reset.
        turn.resets
            .values()
            .copied()
            .chain(snapshot_resets)
            .filter(|reset| *reset > now)
            .max()?
            .checked_add(RESET_GRACE_MS)
    }

    fn arm(&mut self, id: &str, at: u64) -> Option<Arc<AtomicBool>> {
        if self.pending.get(id).is_some_and(|pending| pending.at == at) {
            return None;
        }
        if let Some(old) = self.pending.remove(id) {
            old.cancelled.store(true, Ordering::SeqCst);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.pending.insert(
            id.into(),
            PendingResume {
                at,
                cancelled: cancelled.clone(),
            },
        );
        Some(cancelled)
    }
}

fn reset_millis(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        // Runtime events and Codex snapshots are milliseconds. Accept epoch
        // seconds too, so older runtime snapshots remain usable.
        return if number > 0 && number < 100_000_000_000 {
            number.checked_mul(1_000)
        } else {
            Some(number).filter(|n| *n > 0)
        };
    }
    let millis = crate::codex_app_server::parse_rfc3339_timestamp_millis(value.as_str()?)?;
    u64::try_from(millis).ok()
}

fn is_usage_limit_error(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "rate limit",
        "rate_limit",
        "usage limit",
        "usage_limit",
        "you've hit your limit",
        "you have hit your limit",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub(crate) fn enabled(app: &HostContext) -> bool {
    policy_enabled(app.data_dir_opt().as_deref())
}

fn policy_enabled(data_dir: Option<&std::path::Path>) -> bool {
    data_dir
        .and_then(|dir| std::fs::read_to_string(dir.join("settings.json")).ok())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .is_some_and(|settings| settings["autoResumeAfterRateLimit"] == true)
}

#[derive(Clone, Default)]
pub(crate) struct RateLimitResumeState {
    inner: Arc<Mutex<ResumeState>>,
}

impl RateLimitResumeState {
    pub(crate) fn cancel(&self, id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel(id);
    }

    pub(crate) fn clear(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub(crate) fn observe(
        &self,
        app: &HostContext,
        topic: &str,
        payload: &Value,
        origin: &'static str,
    ) {
        let provider = match origin {
            "node-sidecar" => "claude",
            "codex-app-server" => "codex",
            "rust-usage-poller" | "rust-account" => "",
            _ => return, // A remote client must never resume the host's session locally.
        };
        if topic == "claude:account-changed" {
            self.clear();
            return;
        }
        if !matches!(
            topic,
            "claude:rate-limit"
                | "claude:message"
                | "claude:error"
                | "claude:tool-use"
                | "claude:turn-end"
                | "claude:session-reset"
                | "agent:usage"
        ) {
            return;
        }
        if !enabled(app) {
            self.clear();
            return;
        }
        let snapshots = crate::claude_usage::agent_usage_snapshot();
        let now = crate::session_replay::now_ms();
        let jobs = {
            let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            state.observe(topic, payload, provider);
            let ids: Vec<_> = if topic == "agent:usage" {
                state.turns.keys().cloned().collect()
            } else {
                payload["sessionId"]
                    .as_str()
                    .map(str::to_string)
                    .into_iter()
                    .collect()
            };
            ids.into_iter()
                .filter_map(|id| {
                    let at = state.deadline(&id, &snapshots, now)?;
                    let token = state.arm(&id, at)?;
                    Some((id, at, token))
                })
                .collect::<Vec<_>>()
        };
        for (id, at, cancelled) in jobs {
            let app = app.clone();
            let service = self.clone();
            let _ = std::thread::Builder::new()
                .name("rate-limit-resume".into())
                .spawn(move || {
                    crate::commands::app::log_tauri(
                        &app,
                        &format!("[rate-limit-resume] session={id} waiting until {at}"),
                    );
                    while !cancelled.load(Ordering::SeqCst) {
                        if !enabled(&app) {
                            service.clear();
                            break;
                        }
                        if crate::session_replay::now_ms() >= at {
                            let result = crate::async_rt::block_on(
                                crate::commands::claude::resume_after_rate_limit(
                                    &app, &id, &cancelled,
                                ),
                            );
                            crate::commands::app::log_tauri(
                                &app,
                                &format!("[rate-limit-resume] session={id} result={result:?}"),
                            );
                            break;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    // A replacement wait may already belong to a later turn.
                    let mut state = service.inner.lock().unwrap_or_else(|e| e.into_inner());
                    if state
                        .pending
                        .get(&id)
                        .is_some_and(|job| Arc::ptr_eq(&job.cancelled, &cancelled))
                    {
                        state.cancel(&id);
                    }
                });
        }
    }
}

pub(crate) fn cancel(app: &HostContext, id: &str) {
    if let Some(hub) = app.try_state::<crate::event_hub::RuntimeEventHubState>() {
        hub.rate_limit_resume.cancel(id);
    }
}

pub(crate) fn settings_changed(app: &HostContext) {
    if !enabled(app) {
        if let Some(hub) = app.try_state::<crate::event_hub::RuntimeEventHubState>() {
            hub.rate_limit_resume.clear();
        }
    }
}

pub(crate) fn cancel_for_command(app: &HostContext, channel: &str, params: &Value) {
    if matches!(
        channel,
        "claude:send-message"
            | "claude:stop-session"
            | "claude:abort-session"
            | "claude:reset-session"
            | "claude:rest-session"
            | "claude:resume-session"
            | "claude:reload-session"
            | "claude:start-session"
    ) {
        if let Some(id) = params["sessionId"].as_str() {
            cancel(app, id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_800_000_000_000;
    const RESET: u64 = NOW + 10_000;

    fn rejected(state: &mut ResumeState) {
        state.observe(
            "claude:rate-limit",
            &json!({"sessionId":"s", "info":{
                "status":"rejected", "rateLimitType":"five_hour", "resetsAt":RESET,
            }}),
            "claude",
        );
    }

    fn end(state: &mut ResumeState, reason: &str) {
        state.observe(
            "claude:turn-end",
            &json!({"sessionId":"s", "payload":{"reason":reason}}),
            "claude",
        );
    }

    #[test]
    fn rate_limit_resume_waits_for_end_and_reported_reset() {
        let mut state = ResumeState::default();
        rejected(&mut state);
        assert_eq!(state.deadline("s", &json!({}), NOW), None);
        end(&mut state, "error");
        assert_eq!(
            state.deadline("s", &json!({}), NOW),
            Some(RESET + RESET_GRACE_MS)
        );
        assert_eq!(
            state.deadline("s", &json!({}), RESET),
            None,
            "an old reset must not create an immediate retry loop"
        );
    }

    #[test]
    fn rate_limit_resume_warnings_and_generic_errors_do_not_trigger() {
        let mut state = ResumeState::default();
        state.observe("claude:rate-limit", &json!({"sessionId":"s", "info":{
            "status":"allowed_warning", "rateLimitType":"five_hour", "resetsAt":RESET, "utilization":0.99,
        }}), "claude");
        state.observe(
            "claude:error",
            &json!({"sessionId":"s", "error":"network timeout"}),
            "claude",
        );
        end(&mut state, "error");
        assert_eq!(state.deadline("s", &json!({}), NOW), None);
        state.observe(
            "claude:message",
            &json!({"sessionId":"s", "message":{
                "role":"assistant", "content":"You can inspect the rate limit with /usage.",
            }}),
            "claude",
        );
        end(&mut state, "completed");
        assert_eq!(state.deadline("s", &json!({}), NOW), None);
    }

    #[test]
    fn rate_limit_resume_claude_error_reply_can_end_as_success() {
        let mut state = ResumeState::default();
        state.observe(
            "claude:message",
            &json!({"sessionId":"s", "message":{
                "role":"assistant", "error":"rate_limit", "content":"You've hit your limit",
            }}),
            "claude",
        );
        end(&mut state, "completed");
        let snapshots = json!({"claude":{"fiveHour":{"utilization":1.0,"resetsAt":"2027-01-15T16:00:10+08:00"}}});
        assert_eq!(
            state.deadline("s", &snapshots, NOW),
            Some(RESET + RESET_GRACE_MS)
        );
    }

    #[test]
    fn rate_limit_resume_codex_uses_exhausted_usage_windows() {
        let mut state = ResumeState::default();
        state.observe(
            "claude:turn-end",
            &json!({"sessionId":"s", "payload":{
                "reason":"error", "error":"You've hit your usage limit. Try again later.",
            }}),
            "codex",
        );
        let snapshots = json!({"codex":{
            "fiveHour":{"utilization":1.0,"resetsAt":RESET},
            "sevenDay":{"utilization":0.2,"resetsAt":RESET + 100_000},
        }});
        assert_eq!(
            state.deadline("s", &snapshots, NOW),
            Some(RESET + RESET_GRACE_MS)
        );
        let mut both_full = snapshots;
        both_full["codex"]["sevenDay"]["utilization"] = json!(1.0);
        assert_eq!(
            state.deadline("s", &both_full, NOW),
            Some(RESET + 100_000 + RESET_GRACE_MS)
        );
        both_full["codex"]["stale"] = json!({"reason":"network"});
        assert_eq!(state.deadline("s", &both_full, NOW), None);
    }

    #[test]
    fn rate_limit_resume_ignores_non_blocking_weekly_event() {
        let mut state = ResumeState::default();
        rejected(&mut state);
        state.observe("claude:rate-limit", &json!({"sessionId":"s", "info":{
            "status":"allowed", "rateLimitType":"seven_day", "resetsAt":RESET + 100_000, "utilization":0.2,
        }}), "claude");
        end(&mut state, "completed");
        assert_eq!(
            state.deadline("s", &json!({}), NOW),
            Some(RESET + RESET_GRACE_MS)
        );
    }

    #[test]
    fn rate_limit_resume_recovered_turn_does_not_resume_again() {
        let mut state = ResumeState::default();
        rejected(&mut state);
        state.observe(
            "claude:message",
            &json!({"sessionId":"s", "message":{
                "role":"assistant", "content":"Task finished after the SDK retried.",
            }}),
            "claude",
        );
        end(&mut state, "completed");
        assert_eq!(state.deadline("s", &json!({}), NOW), None);
    }

    #[test]
    fn rate_limit_resume_late_recovery_cancels_an_existing_timer() {
        let mut state = ResumeState::default();
        rejected(&mut state);
        end(&mut state, "error");
        let token = state.arm("s", RESET).unwrap();
        state.observe(
            "claude:message",
            &json!({"sessionId":"s", "message":{
                "role":"assistant", "content":"Task finished after recovery.",
            }}),
            "claude",
        );
        assert!(token.load(Ordering::SeqCst));
        assert!(state.pending.is_empty());
    }

    #[test]
    fn rate_limit_resume_stop_manual_message_and_disable_cancel_waits() {
        for action in ["aborted", "interrupted", "manual", "reset", "disable"] {
            let mut state = ResumeState::default();
            rejected(&mut state);
            end(&mut state, "error");
            let token = state.arm("s", RESET).unwrap();
            match action {
                "manual" => state.observe(
                    "claude:message",
                    &json!({"sessionId":"s", "message":{"role":"user"}}),
                    "claude",
                ),
                "reset" => {
                    state.observe("claude:session-reset", &json!({"sessionId":"s"}), "claude")
                }
                "disable" => state.clear(),
                reason => end(&mut state, reason),
            }
            assert!(
                token.load(Ordering::SeqCst),
                "{action} must cancel the timer"
            );
            assert_eq!(state.deadline("s", &json!({}), NOW), None);
        }
    }

    #[test]
    fn rate_limit_resume_deduplicates_and_replaces_waits() {
        let mut state = ResumeState::default();
        let first = state.arm("s", RESET).unwrap();
        assert!(state.arm("s", RESET).is_none());
        let next = state.arm("s", RESET + 1_000).unwrap();
        assert!(first.load(Ordering::SeqCst));
        assert!(!next.load(Ordering::SeqCst));
    }

    #[test]
    fn rate_limit_resume_missing_reset_never_guesses_five_hours() {
        let mut state = ResumeState::default();
        state.observe(
            "claude:error",
            &json!({"sessionId":"s", "error":"rate limit exceeded"}),
            "claude",
        );
        end(&mut state, "error");
        assert_eq!(state.deadline("s", &json!({}), NOW), None);
        let later_snapshot = json!({"claude":{"fiveHour":{"utilization":1.0,"resetsAt":RESET}}});
        assert_eq!(
            state.deadline("s", &later_snapshot, NOW),
            Some(RESET + RESET_GRACE_MS)
        );
    }

    #[test]
    fn rate_limit_resume_policy_is_strictly_opt_in() {
        assert!(!policy_enabled(None));
        let dir = std::env::temp_dir().join(format!(
            "bat-rate-limit-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!policy_enabled(Some(&dir)));
        for text in [
            "{}",
            "invalid",
            r#"{"autoResumeAfterRateLimit":false}"#,
            r#"{"autoResumeAfterRateLimit":"true"}"#,
        ] {
            std::fs::write(dir.join("settings.json"), text).unwrap();
            assert!(!policy_enabled(Some(&dir)));
        }
        std::fs::write(
            dir.join("settings.json"),
            r#"{"autoResumeAfterRateLimit":true}"#,
        )
        .unwrap();
        assert!(policy_enabled(Some(&dir)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(not(feature = "desktop"))]
    fn rate_limit_resume_headless_sidecar_events_reach_host_once() {
        let dir = std::env::temp_dir().join(format!(
            "bat-rate-limit-headless-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"autoResumeAfterRateLimit":true}"#,
        )
        .unwrap();
        let hub = crate::event_hub::RuntimeEventHubState::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let mut host = crate::host_context::HeadlessHost::new(
            Some(dir.clone()),
            Arc::new(move |topic, _| {
                captured.lock().unwrap().push(topic.to_string());
            }),
        );
        host.manage(hub.clone());
        let app = HostContext::from_headless(Arc::new(host));
        let sink = app.sidecar_emit_sink();
        sink(
            "claude:rate-limit",
            &json!({"sessionId":"s","info":{"status":"rejected","rateLimitType":"five_hour","resetsAt":RESET}}),
        );
        assert!(hub.rate_limit_resume.inner.lock().unwrap().turns["s"].rejected);
        sink(
            "claude:message",
            &json!({"sessionId":"s","message":{"role":"user","content":"continue manually"}}),
        );
        assert!(hub.rate_limit_resume.inner.lock().unwrap().turns.is_empty());
        assert_eq!(
            *events.lock().unwrap(),
            vec!["claude:rate-limit", "claude:message"]
        );
        // Replayed host events on a remote client cannot arm local work.
        hub.rate_limit_resume.observe(
            &app,
            "claude:error",
            &json!({"sessionId":"s","error":"rate limit exceeded"}),
            "rust-remote-client",
        );
        assert!(hub.rate_limit_resume.inner.lock().unwrap().turns.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
