//! One fresh PM turn, using BAT's existing Codex notification/replay adapter.
//! The profile confines the native process; Connector remains operation authority.
use super::*;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;

const PROFILE: &str = "codex-pm-linux-v1";
const CLI_VERSION: &str = "0.160.0";
const CLI_SOURCE: &str = "a956835d020762cb2b570053af06f643a11c0ecc";
const LINE_LIMIT: usize = 1024 * 1024;
const STREAM_LIMIT: usize = 8 * 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
const OUTPUT_LIMIT: usize = 128_000;
const DISABLED: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "multi_agent",
    "multi_agent_v2",
    "agent_message_board",
    "hooks",
    "apps",
    "plugins",
    "remote_plugin",
    "recommended_plugins",
    "tool_suggest",
    "image_generation",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "in_app_browser",
    "code_mode",
    "code_mode_host",
    "code_mode_prewarm",
    "goals",
    "memories",
    "skill_mcp_dependency_install",
    "skill_search",
    "deferred_executor",
    "send_message_to_user_async",
    "token_budget",
    "sleep_tool",
    "current_time_reminder",
];
fn require(ok: bool, code: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(code.into())
    }
}
fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn canonical_json(value: &Value) -> Vec<u8> {
    fn ordered(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys = map.keys().collect::<Vec<_>>();
                keys.sort();
                let mut result = serde_json::Map::new();
                for key in keys {
                    result.insert(key.clone(), ordered(&map[key]));
                }
                Value::Object(result)
            }
            Value::Array(items) => Value::Array(items.iter().map(ordered).collect()),
            _ => value.clone(),
        }
    }
    serde_json::to_vec(&ordered(value)).expect("JSON value serializes")
}
fn hex_digest(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
}
fn checked_options(options: &Value) -> Result<Duration, String> {
    require(
        options["pmRuntimeProfile"] == PROFILE,
        "PM_RUNTIME_PROFILE_UNSUPPORTED",
    )?;
    require(
        is_codex_agent_preset(options),
        "PM_RUNTIME_CODEX_PRESET_REQUIRED",
    )?;
    let fields = [
        "agentPreset",
        "agentType",
        "cwd",
        "workspaceId",
        "workspaceName",
        "profileId",
        "useWorktree",
        "worktreePath",
        "worktreeBranch",
        "model",
        "effort",
        "codexSandboxMode",
        "codexApprovalPolicy",
        "pmRuntimeProfile",
        "pmContextDigest",
        "pmTimeoutMs",
    ];
    require(
        options
            .as_object()
            .is_some_and(|m| m.keys().all(|k| fields.contains(&k.as_str()))),
        "PM_RUNTIME_OPTIONS_UNSUPPORTED",
    )?;
    require(
        options.get("agentType").is_none_or(|v| v == "codex"),
        "PM_RUNTIME_CODEX_PRESET_REQUIRED",
    )?;
    require(
        options
            .get("codexSandboxMode")
            .is_none_or(|v| v == "read-only")
            && options
                .get("codexApprovalPolicy")
                .is_none_or(|v| v == "never"),
        "PM_RUNTIME_BOUNDARY_INVALID",
    )?;
    require(
        hex_digest(&options["pmContextDigest"]),
        "PM_RUNTIME_CONTEXT_INVALID",
    )?;
    require(
        options.get("model").is_none_or(|v| {
            v.as_str()
                .is_some_and(|s| !s.trim().is_empty() && s.len() <= 128)
        }),
        "PM_RUNTIME_MODEL_INVALID",
    )?;
    let millis = match options.get("pmTimeoutMs") {
        None => 300_000,
        Some(v) => v.as_u64().ok_or("PM_RUNTIME_TIMEOUT_INVALID")?,
    };
    require(
        millis > 0 && millis <= 3_600_000,
        "PM_RUNTIME_TIMEOUT_INVALID",
    )?;
    Ok(Duration::from_millis(millis))
}
fn settings(profile: &Path) -> Vec<(String, Value)> {
    let mut values = vec![
        ("approval_policy".into(), json!("never")),
        ("sandbox_mode".into(), json!("read-only")),
        ("web_search".into(), json!("disabled")),
        ("project_doc_max_bytes".into(), json!(0)),
        ("project_root_markers".into(), json!([])),
        ("agents.enabled".into(), json!(false)),
        ("orchestrator.mcp.enabled".into(), json!(false)),
        ("cloud.skills.enabled".into(), json!(false)),
        ("skills.bundled.enabled".into(), json!(false)),
        ("skills.include_instructions".into(), json!(false)),
        (
            "tools.experimental_request_user_input.enabled".into(),
            json!(false),
        ),
        ("tools.update_plan.enabled".into(), json!(false)),
        ("features.skip_host_skill_discovery".into(), json!(true)),
        ("history.persistence".into(), json!("none")),
        ("log_dir".into(), json!(profile.join("log"))),
        ("sqlite_home".into(), json!(profile.join("state"))),
    ];
    values.extend(
        DISABLED
            .iter()
            .map(|name| (format!("features.{name}"), json!(false))),
    );
    values
}
fn validate_config(config: &Value) -> Result<(), String> {
    require(config.is_object(), "PM_RUNTIME_CONFIG_UNVERIFIED")?;
    for (key, expected) in settings(Path::new("/unused")) {
        if key == "log_dir" || key == "sqlite_home" {
            continue;
        }
        let value = key.split('.').fold(config, |v, key| &v[key]);
        // Pinned config/read omits these two internal tool switches.
        if key.starts_with("tools.") && value.is_null() {
            continue;
        }
        require(value == &expected, "PM_RUNTIME_CONFIG_UNVERIFIED")?;
    }
    require(
        config["mcp_servers"] == json!({}),
        "PM_RUNTIME_MCP_UNVERIFIED",
    )?;
    for key in [
        "notify",
        "instructions",
        "developer_instructions",
        "model_instructions_file",
    ] {
        require(
            config
                .get(key)
                .is_none_or(|v| v.is_null() || v == "" || v == &json!([])),
            "PM_RUNTIME_INSTRUCTIONS_UNEXPECTED",
        )?;
    }
    Ok(())
}
fn record_dir(app: &HostContext, session_id: &str) -> Result<PathBuf, String> {
    require(valid_id(session_id), "PM_RUNTIME_SESSION_INVALID")?;
    Ok(app.data_dir()?.join("pm-runtimes").join(digest(session_id)))
}
fn private_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| "PM_RUNTIME_DIRECTORY_UNAVAILABLE".to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err("PM_RUNTIME_PLATFORM_UNSUPPORTED".into())
    }
}
fn atomic_record(path: &Path, record: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(record).map_err(|_| "PM_RUNTIME_RECORD_INVALID")?;
    require(bytes.len() <= LINE_LIMIT, "PM_RUNTIME_RECORD_LIMIT")?;
    let temp = path.with_extension(format!("new-{:032x}", rand::random::<u128>()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(path.parent().unwrap())?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|_| "PM_RUNTIME_RECEIPT_WRITE_FAILED".into())
}
fn read_record(path: &Path, session_id: &str) -> Result<Value, String> {
    require(
        fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() <= LINE_LIMIT as u64),
        "PM_RUNTIME_RECEIPT_UNAVAILABLE",
    )?;
    let bytes = fs::read(path).map_err(|_| "PM_RUNTIME_RECEIPT_UNAVAILABLE")?;
    let record: Value = serde_json::from_slice(&bytes).map_err(|_| "PM_RUNTIME_RECEIPT_INVALID")?;
    require(
        record["sessionId"] == session_id
            && record["profile"] == PROFILE
            && record["profileVersion"] == 1
            && record["runtimeId"].as_str().is_some_and(valid_id),
        "PM_RUNTIME_RECEIPT_INVALID",
    )?;
    Ok(record)
}
/// Retained after stop. Presence is ownership evidence; a missing process field is not death.
#[derive(Default)]
struct NativeItems {
    pending: HashMap<String, String>,
    completed: Vec<Value>,
}
pub(super) struct PmState {
    directory: PathBuf,
    record: Mutex<Value>,
    journal: Mutex<File>,
    started: Instant,
    timeout: Duration,
    stop_lock: Mutex<()>,
    effect_gate: Mutex<()>,
    cancel_requested: AtomicBool,
    #[cfg(test)]
    before_launch: Mutex<Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>>,
    poisoned: AtomicBool,
    stop_requested: AtomicBool,
    items: Mutex<NativeItems>,
}
impl PmState {
    fn create(
        app: &HostContext,
        session: &str,
        options: &Value,
        timeout: Duration,
    ) -> Result<Arc<Self>, String> {
        let started = Instant::now();
        let directory = record_dir(app, session)?;
        let parent = directory.parent().unwrap();
        if !parent.exists() {
            private_dir(parent)?;
        }
        require(!directory.exists(), "PM_RUNTIME_SESSION_ALREADY_USED")?;
        private_dir(&directory)?;
        for name in ["profile", "workspace", "empty-customizations"] {
            private_dir(&directory.join(name))?;
        }
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        let journal = open
            .open(directory.join("events.ndjson"))
            .map_err(|_| "PM_RUNTIME_JOURNAL_UNAVAILABLE")?;
        let record = json!({"sessionId":session,"profile":PROFILE,"profileVersion":1,
            "runtimeId":format!("pm-{:032x}",rand::random::<u128>()),"clientContextDigest":options["pmContextDigest"],
            "batVersion":app.version(),"batSourceRevision":env!("BAT_SOURCE_REVISION"),
            "codexVersion":CLI_VERSION,"codexSourceRevision":CLI_SOURCE,
            "selectedCwd":effective_cwd(options,"pm-start").map_err(|_| "PM_RUNTIME_CWD_INVALID")?,
            "runtimeCwd":directory.join("workspace"),"startedAt":now_millis(),
            "timeoutMs":timeout.as_millis() as u64,"status":"preparing","launchAttempted":false,
            "process":null,"threadId":null,"actualModel":null,"configDigest":null,"attestation":null,
            "send":null,"terminal":null,"exit":null});
        atomic_record(&directory.join("receipt.json"), &record)?;
        Ok(Arc::new(Self {
            directory,
            record: Mutex::new(record),
            journal: Mutex::new(journal),
            started,
            timeout,
            stop_lock: Mutex::new(()),
            effect_gate: Mutex::new(()),
            cancel_requested: AtomicBool::new(false),
            #[cfg(test)]
            before_launch: Mutex::new(None),
            poisoned: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            items: Mutex::new(NativeItems::default()),
        }))
    }
    pub(super) fn view(&self) -> Value {
        let mut value = self.record.lock().expect("PM record lock").clone();
        if self.poisoned.load(Ordering::Acquire) {
            value["receiptPersistence"] = json!("failed");
        }
        value
    }
    fn remaining(&self) -> Result<Duration, String> {
        require(
            !self.stop_requested.load(Ordering::Acquire)
                && !self.cancel_requested.load(Ordering::Acquire),
            "PM_RUNTIME_STOPPED",
        )?;
        self.timeout
            .checked_sub(self.started.elapsed())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| "PM_RUNTIME_DEADLINE_EXPIRED".into())
    }
    fn accept_stop(&self) {
        // Cancellation interrupts a nonblocking partial write; acquiring the gate
        // is the stop linearization point. Never hold it while awaiting an ACK.
        self.cancel_requested.store(true, Ordering::Release);
        let _gate = self.effect_gate.lock().expect("PM effect gate");
        self.stop_requested.store(true, Ordering::Release);
    }
    fn write_frame(&self, connection: &CodexConnection, message: Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(&message).map_err(|_| "PM_RUNTIME_INPUT_INVALID")?;
        bytes.push(b'\n');
        require(bytes.len() <= LINE_LIMIT, "PM_RUNTIME_INPUT_INVALID")?;
        // A contended stdin lock cannot prevent stop from acquiring its gate.
        let mut stdin = connection
            .stdin
            .lock()
            .map_err(|_| "PM_RUNTIME_PIPE_UNAVAILABLE")?;
        let _gate = self
            .effect_gate
            .lock()
            .map_err(|_| "PM_RUNTIME_LOCK_UNAVAILABLE")?;
        let mut written = 0;
        while written < bytes.len() {
            self.remaining()?;
            match stdin.write(&bytes[written..]) {
                Ok(0) => return Err("PM_RUNTIME_PIPE_CLOSED".into()),
                Ok(count) => written += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err("PM_RUNTIME_PIPE_WRITE_FAILED".into()),
            }
        }
        Ok(())
    }
    fn request(
        &self,
        connection: &CodexConnection,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<Value, String> {
        let id = connection.alloc_id();
        if method == "turn/start" {
            require(
                self.view()["send"]["requestId"] == id,
                "PM_RUNTIME_SEND_IDENTITY_MISMATCH",
            )?;
        }
        let (tx, rx) = channel();
        connection.pending.insert(id, tx);
        let result = (|| {
            self.write_frame(connection, json!({"method":method,"id":id,"params":params}))?;
            // The gate and stdin lock have both been released before response wait.
            rx.recv_timeout(self.remaining()?.min(limit))
                .map_err(|_| "PM_RUNTIME_RESPONSE_UNKNOWN".to_string())?
        })();
        if result.is_err() {
            let _ = connection.pending.take(id);
        }
        result
    }
    fn update(&self, edit: impl FnOnce(&mut Value)) -> Result<(), String> {
        let mut record = self
            .record
            .lock()
            .map_err(|_| "PM_RUNTIME_RECORD_UNAVAILABLE")?;
        edit(&mut record);
        let result = atomic_record(&self.directory.join("receipt.json"), &record);
        if result.is_err() {
            self.poisoned.store(true, Ordering::Release);
        }
        result
    }
    fn journal(&self, kind: &str, event: &Value) -> Result<(), String> {
        let mut journal = self
            .journal
            .lock()
            .map_err(|_| "PM_RUNTIME_JOURNAL_UNAVAILABLE")?;
        let value = json!({"kind":kind,"event":event});
        writeln!(journal, "{value}")
            .and_then(|_| journal.sync_all())
            .map_err(|_| {
                self.poisoned.store(true, Ordering::Release);
                "PM_RUNTIME_RECEIPT_WRITE_FAILED".to_string()
            })
    }
    fn native(&self, message: &Value) -> Result<(), String> {
        self.journal("native", message)?;
        require(message.is_object(), "PM_RUNTIME_EVENT_INVALID")?;
        let method = message["method"].as_str();
        require(
            !(method.is_some() && message.get("id").is_some()),
            "PM_RUNTIME_SERVER_REQUEST_FORBIDDEN",
        )?;
        let record = self.view();
        let params = &message["params"];
        if let Some(thread) = params.get("threadId").and_then(Value::as_str) {
            if let Some(expected) = record["threadId"].as_str() {
                require(thread == expected, "PM_RUNTIME_FOREIGN_THREAD")?;
            }
        }
        if let Some(kind) = params["item"]["type"].as_str() {
            require(
                ["userMessage", "agentMessage", "reasoning"].contains(&kind),
                "PM_RUNTIME_TOOL_FORBIDDEN",
            )?;
        }
        require(method != Some("error"), "PM_RUNTIME_NATIVE_ERROR")?;
        if record["send"].is_object() {
            if message["id"] == record["send"]["requestId"] && method.is_none() {
                require(message.get("error").is_none(), "PM_RUNTIME_SEND_FAILED")?;
                let turn = message["result"]["turn"]["id"]
                    .as_str()
                    .ok_or("PM_RUNTIME_TURN_ID_MISSING")?;
                self.bind_turn(turn)?;
                self.update(|r| r["send"]["acknowledged"] = json!(true))?;
            }
            if method == Some("turn/started") {
                let turn = params["turn"]["id"]
                    .as_str()
                    .ok_or("PM_RUNTIME_TURN_ID_MISSING")?;
                self.bind_turn(turn)?;
                self.update(|r| r["send"]["nativeStarted"] = json!(true))?;
            }
            if matches!(method, Some("item/started" | "item/completed")) {
                require(
                    params["threadId"] == record["threadId"]
                        && params["turnId"] == record["send"]["turnId"],
                    "PM_RUNTIME_FOREIGN_ITEM",
                )?;
                let item = &params["item"];
                let id = item["id"]
                    .as_str()
                    .filter(|id| valid_id(id))
                    .ok_or("PM_RUNTIME_ITEM_ID_INVALID")?;
                let kind = item["type"].as_str().ok_or("PM_RUNTIME_ITEM_INVALID")?;
                let mut items = self
                    .items
                    .lock()
                    .map_err(|_| "PM_RUNTIME_ITEMS_UNAVAILABLE")?;
                require(
                    items.completed.len() + items.pending.len() < 256,
                    "PM_RUNTIME_ITEM_LIMIT",
                )?;
                require(
                    !items.completed.iter().any(|v| v["id"] == id),
                    "PM_RUNTIME_DUPLICATE_ITEM",
                )?;
                if method == Some("item/started") {
                    require(!items.pending.contains_key(id), "PM_RUNTIME_DUPLICATE_ITEM")?;
                    items.pending.insert(id.into(), kind.into());
                } else {
                    require(
                        items.pending.remove(id).as_deref() == Some(kind),
                        "PM_RUNTIME_ITEM_START_MISSING",
                    )?;
                    items.completed.push(item.clone());
                }
            }
            if method == Some("item/completed") && params["item"]["type"] == "userMessage" {
                let item = &params["item"];
                require(
                    item["clientId"] == record["send"]["clientMessageId"],
                    "PM_RUNTIME_INPUT_ID_MISMATCH",
                )?;
                let content = item["content"]
                    .as_array()
                    .ok_or("PM_RUNTIME_INPUT_MISMATCH")?;
                require(
                    content.len() == 1 && content[0]["type"] == "text",
                    "PM_RUNTIME_INPUT_MISMATCH",
                )?;
                let text = content[0]["text"]
                    .as_str()
                    .ok_or("PM_RUNTIME_INPUT_MISMATCH")?;
                require(
                    digest(text) == record["send"]["promptDigest"],
                    "PM_RUNTIME_INPUT_MISMATCH",
                )?;
                self.update(|r| r["send"]["observedClientMessageId"] = item["clientId"].clone())?;
            }
            if method == Some("turn/completed") {
                let turn = &params["turn"];
                let turn_id = turn["id"].as_str().ok_or("PM_RUNTIME_TURN_ID_MISSING")?;
                self.bind_turn(turn_id)?;
                require(
                    ["completed", "failed", "interrupted"]
                        .contains(&turn["status"].as_str().unwrap_or("")),
                    "PM_RUNTIME_TERMINAL_INVALID",
                )?;
                require(
                    turn["items"].as_array().is_some_and(|items| {
                        items.iter().all(|item| {
                            ["userMessage", "agentMessage", "reasoning"]
                                .contains(&item["type"].as_str().unwrap_or(""))
                        })
                    }),
                    "PM_RUNTIME_TERMINAL_ITEMS_INVALID",
                )?;
                require(
                    record["terminal"].is_null(),
                    "PM_RUNTIME_DUPLICATE_TERMINAL",
                )?;
                require(
                    params["threadId"] == record["threadId"],
                    "PM_RUNTIME_FOREIGN_THREAD",
                )?;
                let mut terminal = json!({"native":true,"threadId":params["threadId"],"turnId":turn_id,
                    "status":turn["status"],"eventDigest":digest(canonical_json(message)),"event":message});
                if turn["status"] == "completed" {
                    require(
                        record["send"]["acknowledged"] == true
                            && record["send"]["nativeStarted"] == true
                            && record["send"]["observedClientMessageId"]
                                == record["send"]["clientMessageId"],
                        "PM_RUNTIME_COMPLETION_UNPROVEN",
                    )?;
                    let items = self
                        .items
                        .lock()
                        .map_err(|_| "PM_RUNTIME_ITEMS_UNAVAILABLE")?;
                    require(
                        items.pending.is_empty()
                            && items
                                .completed
                                .iter()
                                .filter(|v| v["type"] == "userMessage")
                                .count()
                                == 1,
                        "PM_RUNTIME_ITEMS_INCOMPLETE",
                    )?;
                    let assistant = items
                        .completed
                        .iter()
                        .rev()
                        .find(|v| {
                            v["type"] == "agentMessage"
                                && (v["phase"].is_null() || v["phase"] == "final_answer")
                                && v["text"].as_str().is_some_and(|s| !s.trim().is_empty())
                        })
                        .ok_or("PM_RUNTIME_FINAL_OUTPUT_MISSING")?;
                    require(
                        assistant["text"].as_str().unwrap().chars().count() <= OUTPUT_LIMIT,
                        "PM_RUNTIME_OUTPUT_LIMIT",
                    )?;
                    let view = turn["itemsView"].as_str().unwrap_or("full");
                    require(
                        (view == "summary" && turn["items"] == json!([assistant]))
                            || (view == "full" && turn["items"] == json!(items.completed)),
                        "PM_RUNTIME_TERMINAL_MISMATCH",
                    )?;
                    terminal["nativeMessageId"] = assistant["id"].clone();
                    terminal["outputDigest"] = json!(digest(assistant["text"].as_str().unwrap()));
                }
                self.update(|r| {
                    r["terminal"] = terminal;
                    if r["exit"]["confirmedDead"] != true {
                        r["status"] = json!("native_terminal");
                    }
                })?;
            }
        } else {
            require(
                !matches!(
                    method,
                    Some("turn/started" | "turn/completed" | "item/started" | "item/completed")
                ),
                "PM_RUNTIME_NOT_FRESH",
            )?;
        }
        Ok(())
    }
    fn bind_turn(&self, turn: &str) -> Result<(), String> {
        require(valid_id(turn), "PM_RUNTIME_TURN_ID_INVALID")?;
        let record = self.view();
        require(
            record["send"]["turnId"].is_null() || record["send"]["turnId"] == turn,
            "PM_RUNTIME_FOREIGN_TURN",
        )?;
        self.update(|r| r["send"]["turnId"] = json!(turn))
    }
}

#[cfg(target_os = "linux")]
fn process_identity(pid: u32) -> Result<Value, String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|_| "PM_RUNTIME_PROCESS_IDENTITY_UNAVAILABLE")?;
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .ok_or("PM_RUNTIME_PROCESS_IDENTITY_UNAVAILABLE")?
        .1
        .split_whitespace()
        .collect();
    let ticks: u64 = fields
        .get(19)
        .ok_or("PM_RUNTIME_PROCESS_IDENTITY_UNAVAILABLE")?
        .parse()
        .map_err(|_| "PM_RUNTIME_PROCESS_IDENTITY_UNAVAILABLE")?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|_| "PM_RUNTIME_PROCESS_IDENTITY_UNAVAILABLE")?;
    Ok(json!({"pid":pid,"startTicks":ticks,"bootIdDigest":digest(boot.trim())}))
}
fn inspect_process(identity: &Value) -> Value {
    #[cfg(target_os = "linux")]
    {
        if let Some(pid) = identity["pid"]
            .as_u64()
            .filter(|p| *p > 0 && *p <= u32::MAX as u64)
        {
            if identity["startTicks"].as_u64().is_some() && hex_digest(&identity["bootIdDigest"]) {
                if let Ok(boot) = fs::read_to_string("/proc/sys/kernel/random/boot_id") {
                    if identity["bootIdDigest"] != digest(boot.trim()) {
                        return json!({"state":"dead","proof":"host_rebooted"});
                    }
                    match process_identity(pid as u32) {
                        Ok(current) if current != *identity => {
                            return json!({"state":"dead","proof":"pid_reused"})
                        }
                        Ok(_) => {
                            let dead = fs::read_to_string(format!("/proc/{pid}/stat"))
                                .ok()
                                .and_then(|s| {
                                    s.rsplit_once(')').map(|(_, f)| {
                                        f.split_whitespace().next().unwrap_or("").to_string()
                                    })
                                })
                                .is_some_and(|s| s == "Z" || s == "X");
                            return json!({"state":if dead {"dead"} else {"alive"},"proof":if dead {"original_process_exited"} else {"original_identity_matches"}});
                        }
                        Err(_)
                            if fs::metadata(format!("/proc/{pid}")).is_err_and(|error| {
                                error.kind() == std::io::ErrorKind::NotFound
                            }) =>
                        {
                            return json!({"state":"dead","proof":"original_pid_absent"})
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    json!({"state":"unknown","proof":"identity_unavailable"})
}
fn resolve_program(name: &str) -> Result<PathBuf, String> {
    let candidate = if Path::new(name).is_absolute() {
        Some(PathBuf::from(name))
    } else {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|p| p.join(name))
                .find(|p| p.is_file())
        })
    };
    candidate
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| "PM_RUNTIME_EXECUTABLE_UNAVAILABLE".into())
}
fn file_digest(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|_| "PM_RUNTIME_BINARY_IDENTITY_UNAVAILABLE")?;
    let mut hash = Sha256::new();
    let mut total = 0usize;
    let mut bytes = [0; 65536];
    loop {
        let size = file
            .read(&mut bytes)
            .map_err(|_| "PM_RUNTIME_BINARY_IDENTITY_UNAVAILABLE")?;
        if size == 0 {
            break;
        }
        total += size;
        require(
            total <= 512 * 1024 * 1024,
            "PM_RUNTIME_BINARY_IDENTITY_LIMIT",
        )?;
        hash.update(&bytes[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
#[cfg(target_os = "linux")]
fn kill_owned(connection: &CodexConnection, identity: &Value) {
    // This PID is a live owned Child and its process-group leader, never a caller PID.
    let mut child = connection.child.lock().expect("PM child lock");
    if matches!(child.try_wait(), Ok(None))
        && process_identity(connection.pid).is_ok_and(|current| current == *identity)
    {
        unsafe {
            libc::kill(-(connection.pid as i32), libc::SIGKILL);
        }
        let _ = child.kill();
    }
}
fn finish(pm: &PmState, connection: &CodexConnection, action: &str) -> Value {
    pm.accept_stop();
    let _stop = pm.stop_lock.lock().expect("PM stop lock");
    if pm.view()["exit"]["confirmedDead"] == true {
        return pm.view();
    }
    #[cfg(target_os = "linux")]
    kill_owned(connection, &pm.view()["process"]);
    let until = Instant::now() + Duration::from_secs(2);
    let mut exited = None;
    while Instant::now() < until {
        exited = connection
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten());
        if exited.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let exit = if let Some(status) = exited {
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal: Option<i32> = None;
        json!({"confirmedDead":true,"exitCode":status.code(),"signal":signal,"action":action,"process":pm.view()["process"]})
    } else {
        json!({"confirmedDead":false,"exitCode":null,"signal":null,"action":action,"process":pm.view()["process"]})
    };
    let _ = pm.journal("process_exit", &exit);
    let _ = pm.update(|r| {
        r["exit"] = exit;
        r["status"] = json!(if r["exit"]["confirmedDead"] == true {
            "exited"
        } else {
            "exit_unknown"
        });
    });
    for tx in connection.pending.drain_all() {
        let _ = tx.send(Err("PM_RUNTIME_PROCESS_ENDED".into()));
    }
    pm.view()
}
#[cfg(target_os = "linux")]
fn verify_version(executable: &Path, deadline: Duration) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(executable)
        .arg("--version")
        .env_clear()
        .envs(std::env::vars_os().filter(|(k, _)| k == "PATH" || k == "HOME"))
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "PM_RUNTIME_VERSION_UNAVAILABLE")?;
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(129).read_to_end(&mut bytes);
        let _ = tx.send((result, bytes));
    });
    let result = rx.recv_timeout(deadline.min(Duration::from_secs(5)));
    let valid = result.is_ok_and(|(result, bytes)| {
        result.is_ok()
            && bytes.len() <= 128
            && String::from_utf8_lossy(&bytes).trim() == format!("codex-cli {CLI_VERSION}")
    });
    let until = Instant::now() + Duration::from_millis(100);
    let mut status = child.try_wait().ok().flatten();
    while status.is_none() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1));
        status = child.try_wait().ok().flatten();
    }
    if status.is_none() {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
    }
    let _ = child.wait();
    require(
        valid && status.is_some_and(|s| s.success()),
        "PM_RUNTIME_VERSION_UNSUPPORTED",
    )
}
#[cfg(target_os = "linux")]
fn sandbox_command(
    pm: &PmState,
    executable: &Path,
    bwrap: &Path,
    host_home: &Path,
    native_home: &Path,
    auth: &File,
    offline: bool,
) -> Result<Command, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let profile = pm.directory.join("profile");
    let workspace = pm.directory.join("workspace");
    let config = settings(&profile)
        .into_iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect::<String>();
    fs::write(profile.join("config.toml"), config).map_err(|_| "PM_RUNTIME_CONFIG_WRITE_FAILED")?;
    File::create(profile.join("auth.json")).map_err(|_| "PM_RUNTIME_AUTH_BIND_FAILED")?;
    let mut command = Command::new(bwrap);
    // Receipts/journal remain read-only inside the namespace. Only native state
    // and the fresh, empty working directory are writable.
    command
        .args(["--ro-bind", "/", "/", "--bind"])
        .arg(&profile)
        .arg(&profile)
        .arg("--bind")
        .arg(&workspace)
        .arg(&workspace)
        .arg("--bind")
        .arg(&profile)
        .arg(native_home);
    for path in [host_home.join(".agents"), host_home.join(".codex")] {
        if path.is_dir() && path.canonicalize().ok().as_deref() != Some(native_home) {
            command
                .arg("--ro-bind")
                .arg(pm.directory.join("empty-customizations"))
                .arg(path);
        }
    }
    let fd = auth.as_raw_fd();
    command
        .arg("--ro-bind")
        .arg(format!("/proc/self/fd/{fd}"))
        .arg(native_home.join("auth.json"))
        .args(["--unshare-pid", "--die-with-parent", "--chdir"])
        .arg(&workspace);
    if offline {
        command.arg("--unshare-net");
    }
    command
        .arg(executable)
        .args(["app-server", "--strict-config", "--stdio"]);
    let allowed = [
        "PATH",
        "HOME",
        "CODEX_HOME",
        "LANG",
        "LC_ALL",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "NO_PROXY",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_EXTRA_CA_CERTS",
    ];
    command.env_clear().envs(
        std::env::vars_os()
            .filter(|(key, _)| key.to_str().is_some_and(|key| allowed.contains(&key))),
    );
    command
        .current_dir(&workspace)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Only the child inherits this read-only descriptor. No credential bytes
    // are copied, logged or exposed through the IPC envelope.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command)
}
impl CodexAppServerState {
    pub(super) fn pm_state(&self, id: &str) -> Option<Arc<PmState>> {
        self.inner.session_runtimes.lock().ok()?.get(id)?.pm.clone()
    }
    pub fn is_pm_session(&self, id: &str) -> bool {
        self.pm_state(id).is_some()
    }
    pub fn retained_pm_session(&self, app: &HostContext, id: &str) -> bool {
        record_dir(app, id).is_ok_and(|p| p.exists())
    }
    pub(super) fn reject_pm_session(&self, app: &HostContext, id: &str) -> Result<(), BridgeError> {
        if self.pm_state(id).is_some() || record_dir(app, id).is_ok_and(|path| path.exists()) {
            return Err(bridge_error("PM_RUNTIME_IMMUTABLE"));
        }
        Ok(())
    }
    pub fn get_pm_runtime(&self, app: &HostContext, id: &str) -> Result<Value, String> {
        let mut record = if let Some(pm) = self.pm_state(id) {
            pm.view()
        } else {
            read_record(&record_dir(app, id)?.join("receipt.json"), id)?
        };
        // Observation only, including after a BAT restart. Missing receipt or
        // missing identity cannot establish that a process did not launch.
        record["processObservation"] = inspect_process(&record["process"]);
        Ok(json!({"sessionId":id,"pmRuntime":record}))
    }
    fn pm_stop_with_connection(&self, id: &str, action: &str) -> Value {
        let owner = self.for_session(id);
        let Some(pm) = owner.runtime.as_ref().and_then(|r| r.pm.as_ref()) else {
            return json!({"ok":false,"error":"PM_RUNTIME_NOT_OWNED"});
        };
        pm.accept_stop();
        let connection = owner
            .connection_slot()
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let record = if let Some(connection) = connection {
            finish(pm, &connection, action)
        } else {
            pm.view()
        };
        if record["exit"]["confirmedDead"] == true {
            if let Some(session) = self
                .inner
                .sessions
                .lock()
                .expect("PM session lock")
                .get_mut(id)
            {
                session.is_running = false;
                session.active_turn_id = None;
                session.active_turn_key = None;
                set_runtime_status(session, "stopped", "Dedicated PM process has exited.");
            }
        }
        json!({"ok":record["exit"]["confirmedDead"]==true,"sessionId":id,"pmRuntime":record})
    }
    pub(super) fn stop_pm_session(&self, id: &str, action: &str) -> Value {
        // Does not wait on the turn RPC lock: stopping must break blocked stdin
        // or a lost turn/start response. There is never a second send.
        self.pm_stop_with_connection(id, action)
    }
    pub(super) fn start_pm_session(
        &self,
        app: &HostContext,
        id: String,
        options: Value,
    ) -> Result<Value, BridgeError> {
        let timeout = checked_options(&options).map_err(bridge_error)?;
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (app, id, timeout);
            return Err(bridge_error("PM_RUNTIME_PLATFORM_UNSUPPORTED"));
        }
        #[cfg(target_os = "linux")]
        {
            let operation_lock = self.turn_operation_lock(&id);
            let _guard = operation_lock
                .lock()
                .map_err(|_| bridge_error("PM_RUNTIME_LOCK_UNAVAILABLE"))?;
            require(
                !self.is_owned(&id) && self.pm_state(&id).is_none(),
                "PM_RUNTIME_SESSION_ALREADY_USED",
            )
            .map_err(bridge_error)?;
            let pm = PmState::create(app, &id, &options, timeout).map_err(bridge_error)?;
            let runtime = Arc::new(CodexRuntime {
                pm: Some(pm.clone()),
                ..Default::default()
            });
            {
                let mut runtimes = self
                    .inner
                    .session_runtimes
                    .lock()
                    .expect("PM runtimes lock");
                let live = runtimes
                    .values()
                    .filter(|r| {
                        r.pm.as_ref().is_some_and(|p| {
                            let value = p.view();
                            value["exit"]["confirmedDead"] != true
                                && !(value["status"] == "failed"
                                    && value["launchAttempted"] == false)
                        })
                    })
                    .count();
                if live >= 16 {
                    pm.update(|r| {
                        r["status"] = json!("failed");
                        r["failureReason"] = json!("PM_RUNTIME_CAPACITY");
                    })
                    .map_err(bridge_error)?;
                    return Err(bridge_error("PM_RUNTIME_CAPACITY"));
                }
                runtimes.insert(id.clone(), runtime.clone());
            }
            let model = options["model"]
                .as_str()
                .unwrap_or(DEFAULT_CODEX_MODEL)
                .to_string();
            let cwd = effective_cwd(&options, "pm-start")?;
            let mut session = pm_session(&id, &cwd, &model);
            session.fast_mode_data_dir = app.data_dir_opt();
            self.inner
                .sessions
                .lock()
                .expect("PM sessions lock")
                .insert(id.clone(), session);
            let owner = Self {
                inner: self.inner.clone(),
                runtime: Some(runtime),
            };
            let result = owner.launch_pm(app, &id, &model, &pm);
            match result {
                Ok(()) => Ok(
                    json!({"ok":true,"sessionId":id,"sdkSessionId":pm.view()["threadId"],"pmRuntime":pm.view()}),
                ),
                Err(code) => {
                    let _ = pm.update(|r| {
                        r["failureReason"] = json!(code);
                        r["status"] = json!("failed");
                    });
                    owner.pm_stop_with_connection(&id, "preflight_failed");
                    Err(bridge_error(code))
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    fn launch_pm(
        &self,
        app: &HostContext,
        id: &str,
        model: &str,
        pm: &Arc<PmState>,
    ) -> Result<(), String> {
        use std::os::unix::fs::OpenOptionsExt;
        require(
            !["config.toml", "managed_config.toml", "requirements.toml"]
                .iter()
                .any(|name| Path::new("/etc/codex").join(name).exists()),
            "PM_RUNTIME_MANAGED_CONFIG_UNSUPPORTED",
        )?;
        let executable = resolve_program(
            &std::env::var("BAT_PM_CODEX_PATH").unwrap_or_else(|_| "codex".into()),
        )?;
        let bwrap = resolve_program("bwrap")?;
        verify_version(&executable, pm.remaining()?)?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(|p| p.canonicalize().ok())
            .ok_or("PM_RUNTIME_HOME_UNAVAILABLE")?;
        let native = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
            .canonicalize()
            .map_err(|_| "PM_RUNTIME_HOME_UNAVAILABLE")?;
        require(
            native.is_dir() && native != Path::new("/") && native != home,
            "PM_RUNTIME_HOME_UNAVAILABLE",
        )?;
        let auth = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(native.join("auth.json"))
            .map_err(|_| "PM_RUNTIME_AUTH_UNAVAILABLE")?;
        require(
            auth.metadata().is_ok_and(|m| m.is_file()),
            "PM_RUNTIME_AUTH_INVALID",
        )?;
        let cli_digest = file_digest(&executable)?;
        let bat_digest = file_digest(
            &std::env::current_exe().map_err(|_| "PM_RUNTIME_BINARY_IDENTITY_UNAVAILABLE")?,
        )?;
        let command = sandbox_command(pm, &executable, &bwrap, &home, &native, &auth, false)?;
        pm.update(|r| {
            r["codexBinaryDigest"] = json!(cli_digest);
            r["batBinaryDigest"] = json!(bat_digest);
        })?;
        self.run_pm_command(app, id, model, pm, command, Some(auth))
    }
    #[cfg(target_os = "linux")]
    fn run_pm_command(
        &self,
        app: &HostContext,
        id: &str,
        model: &str,
        pm: &Arc<PmState>,
        mut command: Command,
        auth: Option<File>,
    ) -> Result<(), String> {
        pm.remaining()?;
        #[cfg(test)]
        if let Some((arrived, proceed)) = pm.before_launch.lock().unwrap().take() {
            arrived.wait();
            proceed.wait();
        }
        let gate = pm
            .effect_gate
            .lock()
            .map_err(|_| "PM_RUNTIME_LOCK_UNAVAILABLE")?;
        pm.remaining()?;
        pm.update(|r| r["launchAttempted"] = json!(true))?;
        let mut child = command.spawn().map_err(|_| "PM_RUNTIME_SPAWN_FAILED")?;
        drop(auth);
        let pid = child.id();
        let identity = match process_identity(pid) {
            Ok(v) => v,
            Err(code) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(code);
            }
        };
        let stdin = child.stdin.take().ok_or("PM_RUNTIME_PIPE_MISSING")?;
        use std::os::fd::AsRawFd;
        let flags = unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_GETFL) };
        if flags == -1
            || unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                == -1
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err("PM_RUNTIME_NONBLOCKING_PIPE_UNAVAILABLE".into());
        }
        let stdout = child.stdout.take().ok_or("PM_RUNTIME_PIPE_MISSING")?;
        let stderr = child.stderr.take().ok_or("PM_RUNTIME_PIPE_MISSING")?;
        let pending = Arc::new(PendingTable::default());
        let connection = Arc::new(CodexConnection {
            stdin: Mutex::new(stdin),
            next_id: AtomicU64::new(0),
            pending: pending.clone(),
            child: Mutex::new(child),
            pid,
            auth_account_id: None,
            binary_identity: pm.view()["codexBinaryDigest"]
                .as_str()
                .unwrap_or("test")
                .into(),
        });
        *self.connection_slot().lock().expect("PM connection lock") = Some(connection.clone());
        pm.update(|r| r["process"] = identity)?;
        drop(gate);
        self.start_pm_readers(app, id, pm, connection.clone(), stdout, stderr);
        let watch_pm = pm.clone();
        let watch_connection = Arc::downgrade(&connection);
        std::thread::spawn(move || loop {
            if watch_pm.view()["exit"]["confirmedDead"] == true {
                break;
            }
            let wait = match watch_pm.remaining() {
                Ok(d) => d.min(Duration::from_millis(100)),
                Err(_) => {
                    if let Some(connection) = watch_connection.upgrade() {
                        finish(&watch_pm, &connection, "deadline");
                    }
                    break;
                }
            };
            std::thread::sleep(wait);
        });
        let request = |method: &str, params: Value| {
            pm.request(&connection, method, params, Duration::from_secs(20))
        };
        request(
            "initialize",
            json!({"clientInfo":{"name":"better_agent_terminal_pm","version":app.version()},"capabilities":{"experimentalApi":true}}),
        )?;
        pm.write_frame(&connection, json!({"method":"initialized","params":{}}))?;
        let cwd = pm.directory.join("workspace");
        let config = request("config/read", json!({"cwd":cwd,"includeLayers":true}))?;
        validate_config(&config["config"])?;
        let skills = request("skills/list", json!({"cwds":[cwd],"forceReload":true}))?;
        require(
            skills["data"].as_array().is_some_and(|d| {
                d.len() == 1 && d[0]["skills"] == json!([]) && d[0]["errors"] == json!([])
            }),
            "PM_RUNTIME_SKILLS_UNVERIFIED",
        )?;
        let started = request(
            "thread/start",
            json!({"cwd":cwd,"model":model,"ephemeral":true,"environments":[],
            "selectedCapabilityRoots":[],"dynamicTools":[],"sandbox":"read-only","approvalPolicy":"never","allowProviderModelFallback":false}),
        )?;
        validate_thread(&started, &cwd, model)?;
        let thread = started["thread"]["id"].as_str().unwrap().to_string();
        pm.update(|r| {
            r["threadId"] = json!(thread);
            r["actualModel"] = started["model"].clone();
        })?;
        let mcp = request(
            "mcpServerStatus/list",
            json!({"threadId":thread,"limit":100}),
        )?;
        require(
            mcp["data"] == json!([]) && mcp["nextCursor"].is_null(),
            "PM_RUNTIME_MCP_UNVERIFIED",
        )?;
        pm.remaining()?;
        require(self.live_connection().is_some(), "PM_RUNTIME_PROCESS_ENDED")?;
        pm.update(|r|{r["configDigest"]=json!(digest(canonical_json(&config["config"])));r["status"]=json!("ready");
            r["attestation"]=json!({"optionsConfirmed":true,"isolatedConfigConfirmed":true,"externalToolsDisabled":true,
                "environmentAccess":false,"skills":[],"mcpServers":[],"toolCatalog":"not_exposed_by_protocol",
                "internalToolOptionsReadback":"not_exposed_by_protocol","freshThread":true,"initialNumTurns":0,
                "authFileReadable":true,"authValidity":"unverified","modelExecutionVerified":false});})?;
        self.inner
            .thread_to_session
            .lock()
            .expect("PM thread map")
            .insert(thread.clone(), id.to_string());
        self.inner
            .sessions
            .lock()
            .expect("PM sessions")
            .get_mut(id)
            .unwrap()
            .thread_id = Some(thread);
        emit(
            app,
            "claude:status",
            id,
            "meta",
            self.inner
                .sessions
                .lock()
                .expect("PM sessions")
                .get(id)
                .unwrap()
                .metadata(),
        );
        Ok(())
    }
    #[cfg(target_os = "linux")]
    fn start_pm_readers(
        &self,
        app: &HostContext,
        id: &str,
        pm: &Arc<PmState>,
        connection: Arc<CodexConnection>,
        stdout: std::process::ChildStdout,
        stderr: std::process::ChildStderr,
    ) {
        let state = self.clone();
        let app = app.clone();
        let id = id.to_string();
        let pm = pm.clone();
        let stderr_pm = pm.clone();
        let stderr_connection = connection.clone();
        std::thread::spawn(move || {
            let mut reader = stderr;
            let mut total = 0;
            let mut bytes = [0; 4096];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => {
                        total += size;
                        if total > STDERR_LIMIT {
                            finish(&stderr_pm, &stderr_connection, "stderr_limit");
                            break;
                        }
                    }
                }
            }
        });
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut total = 0;
            let mut output = 0;
            loop {
                let mut bytes = Vec::new();
                let result = reader
                    .by_ref()
                    .take((LINE_LIMIT + 1) as u64)
                    .read_until(b'\n', &mut bytes);
                if result.is_err() || bytes.is_empty() {
                    break;
                }
                total += bytes.len();
                if bytes.len() > LINE_LIMIT || total > STREAM_LIMIT || bytes.last() != Some(&b'\n')
                {
                    finish(&pm, &connection, "stdout_limit");
                    break;
                }
                let Ok(message) = serde_json::from_slice::<Value>(&bytes) else {
                    finish(&pm, &connection, "invalid_json");
                    break;
                };
                if let Some(delta) = message["params"]["delta"].as_str() {
                    output += delta.chars().count();
                }
                if output > OUTPUT_LIMIT {
                    finish(&pm, &connection, "output_limit");
                    break;
                }
                if let Err(code) = pm.native(&message) {
                    let _ = pm.update(|r| r["failureReason"] = json!(code));
                    finish(&pm, &connection, "native_boundary_failed");
                    break;
                }
                let terminal = message["method"] == "turn/completed";
                handle_server_message(
                    &app,
                    &state,
                    &connection.pending,
                    &Arc::downgrade(&connection),
                    message,
                );
                if terminal {
                    finish(&pm, &connection, "native_terminal");
                    break;
                }
            }
            let record = finish(&pm, &connection, "stream_ended");
            if let Some(session) = state
                .inner
                .sessions
                .lock()
                .expect("PM sessions")
                .get_mut(&id)
            {
                if record["exit"]["confirmedDead"] == true {
                    session.is_running = false;
                }
            }
            if let Some(session) = state.inner.sessions.lock().expect("PM sessions").get(&id) {
                emit(&app, "claude:status", &id, "meta", session.metadata());
            }
        });
    }
    pub fn send_pm_message(
        &self,
        app: &HostContext,
        id: &str,
        prompt: &str,
        images: &[String],
        params: &Value,
    ) -> Result<Value, BridgeError> {
        let lock = self.turn_operation_lock(id);
        let _guard = lock
            .lock()
            .map_err(|_| bridge_error("PM_RUNTIME_LOCK_UNAVAILABLE"))?;
        let pm = self
            .pm_state(id)
            .ok_or_else(|| bridge_error("PM_RUNTIME_NOT_OWNED"))?;
        let result = (|| -> Result<Value, String> {
            pm.remaining()?;
            require(
                !pm.poisoned.load(Ordering::Acquire),
                "PM_RUNTIME_RECEIPT_WRITE_FAILED",
            )?;
            let record = pm.view();
            require(
                record["status"] == "ready" && record["send"].is_null() && record["exit"].is_null(),
                "PM_RUNTIME_ALREADY_SENT_OR_STOPPED",
            )?;
            require(
                params["pmRuntimeId"] == record["runtimeId"]
                    && params["pmContextDigest"] == record["clientContextDigest"],
                "PM_RUNTIME_SEND_IDENTITY_MISMATCH",
            )?;
            let client = params["clientMessageId"]
                .as_str()
                .filter(|v| valid_id(v))
                .ok_or("PM_RUNTIME_CLIENT_MESSAGE_ID_REQUIRED")?;
            require(
                images.is_empty() && !prompt.trim().is_empty() && prompt.len() <= LINE_LIMIT / 2,
                "PM_RUNTIME_INPUT_INVALID",
            )?;
            let native_params = json!({"threadId":record["threadId"],"clientUserMessageId":client,
                "input":[{"type":"text","text":prompt}],"environments":[],"model":record["actualModel"],
                "approvalPolicy":"never","sandboxPolicy":{"type":"readOnly","networkAccess":false}});
            require(
                serde_json::to_vec(&native_params)
                    .map_err(|_| "PM_RUNTIME_INPUT_INVALID")?
                    .len()
                    + 100
                    <= LINE_LIMIT,
                "PM_RUNTIME_INPUT_INVALID",
            )?;
            let owner = self.for_session(id);
            let connection = owner.live_connection().ok_or("PM_RUNTIME_PROCESS_ENDED")?;
            let request_id = connection.next_id.load(Ordering::Acquire) + 1;
            pm.update(|r|{r["send"]=json!({"attempted":true,"promptDigest":digest(prompt),"clientMessageId":client,
                "requestId":request_id,"turnId":null,"acknowledged":false,"nativeStarted":false,"observedClientMessageId":null});r["status"]=json!("sent");})?;
            let turn_key = format!("pm-{client}");
            {
                let mut sessions = self.inner.sessions.lock().expect("PM session lock");
                let session = sessions.get_mut(id).ok_or("PM_RUNTIME_SESSION_MISSING")?;
                require(
                    session.num_turns == 0
                        && !session.thread_has_started_turn
                        && !session.is_running,
                    "PM_RUNTIME_NOT_FRESH",
                )?;
                session.num_turns = 1;
                session.thread_has_started_turn = true;
                session.is_running = true;
                session.active_turn_key = Some(turn_key.clone());
                session.last_turn_started_at = Some(Instant::now());
                let mut message = make_user_message(id, prompt, 0);
                message["clientMessageId"] = json!(client);
                session.messages.push(message.clone());
                emit(app, "claude:message", id, "message", message);
                emit(app, "claude:status", id, "meta", session.metadata());
            }
            // Slot and exact request ID are fsynced before the sole write. No
            // ensure/reconnect/thread-resume/retry is reachable on this path.
            let response = pm.request(
                &connection,
                "turn/start",
                native_params,
                Duration::from_secs(60),
            );
            match response {
                Ok(response) => {
                    let turn = response["turn"]["id"]
                        .as_str()
                        .ok_or("PM_RUNTIME_TURN_ID_MISSING")?;
                    pm.bind_turn(turn)?;
                    if let Some(session) = self
                        .inner
                        .sessions
                        .lock()
                        .expect("PM session lock")
                        .get_mut(id)
                    {
                        apply_turn_start_response(session, &turn_key, turn);
                    }
                    Ok(
                        json!({"ok":true,"sessionId":id,"clientMessageId":client,"pmRuntime":pm.view()}),
                    )
                }
                Err(_) => {
                    finish(&pm, &connection, "send_unknown");
                    Err("PM_RUNTIME_SEND_UNKNOWN_NO_RETRY".into())
                }
            }
        })();
        result.map_err(bridge_error)
    }
}
fn validate_thread(started: &Value, cwd: &Path, model: &str) -> Result<(), String> {
    require(
        started["sandbox"] == json!({"type":"readOnly","networkAccess":false})
            && started["approvalPolicy"] == "never"
            && started["instructionSources"] == json!([])
            && started["cwd"] == json!(cwd)
            && started["thread"]["environments"] == json!([])
            && started["thread"]["turns"] == json!([])
            && started["thread"]["ephemeral"] == true
            && started["thread"]["id"].as_str().is_some_and(valid_id)
            && started["model"] == model,
        "PM_RUNTIME_THREAD_UNVERIFIED",
    )
}
fn pm_session(id: &str, cwd: &str, model: &str) -> CodexSession {
    CodexSession {
        session_id: id.into(),
        thread_id: None,
        thread_has_started_turn: false,
        cwd: cwd.into(),
        model: model.into(),
        sandbox_mode: "read-only".into(),
        approval_policy: "never".into(),
        effort: "medium".into(),
        fast_mode: false,
        fast_mode_epoch: 0,
        fast_mode_data_dir: None,
        context_window: codex_context_window_for_model(model),
        start_time: Instant::now(),
        active_turn_id: None,
        active_turn_key: None,
        active_turn_started: false,
        assistant_text: String::new(),
        thinking_text: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        last_input_tokens: 0,
        last_output_tokens: 0,
        last_cache_read_tokens: 0,
        num_turns: 0,
        input_state: NativeInputState::default(),
        last_turn_started_at: None,
        last_turn_first_token_ms: None,
        last_turn_duration_ms: None,
        messages: vec![],
        temporary_image_paths: vec![],
        command_outputs: HashMap::new(),
        command_output_last_emit: HashMap::new(),
        runtime_status: None,
        runtime_message: None,
        runtime_status_started_at: None,
        is_running: false,
        is_resting: false,
        abort_requested: false,
        ignored_turn_ids: vec![],
        warned_stale_turn_ids: vec![],
        stale_turn_event_counts: HashMap::new(),
    }
}

#[cfg(all(test, target_os = "linux", not(feature = "desktop")))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    struct Rig {
        app: HostContext,
        state: CodexAppServerState,
        pm: Arc<PmState>,
        root: PathBuf,
        events: Arc<Mutex<Vec<(String, Value)>>>,
    }
    fn options() -> Value {
        json!({"agentPreset":"codex-agent-worktree","cwd":"/logical/repository","codexSandboxMode":"read-only",
        "codexApprovalPolicy":"never","pmRuntimeProfile":PROFILE,"pmContextDigest":"a".repeat(64),"pmTimeoutMs":3000})
    }
    fn nested_config() -> Value {
        let mut config = json!({"mcp_servers":{}});
        for (key, value) in settings(Path::new("/unused")) {
            let parts: Vec<_> = key.split('.').collect();
            let mut next = &mut config;
            for part in &parts[..parts.len() - 1] {
                if !next[*part].is_object() {
                    next[*part] = json!({});
                }
                next = &mut next[*part];
            }
            next[parts[parts.len() - 1]] = value;
        }
        config
    }
    impl Rig {
        fn new(mode: &str, timeout: u64) -> Self {
            let (rig, command) = Self::unstarted(mode, timeout);
            let owner = rig.state.for_session("pm-session");
            let result =
                owner.run_pm_command(&rig.app, "pm-session", "test-model", &rig.pm, command, None);
            if mode == "bad-config" {
                assert_eq!(result.unwrap_err(), "PM_RUNTIME_CONFIG_UNVERIFIED");
                rig.state
                    .stop_pm_session("pm-session", "test_failed_preflight");
            } else {
                result.unwrap();
            }
            rig
        }
        fn unstarted(mode: &str, timeout: u64) -> (Self, Command) {
            let root =
                std::env::temp_dir().join(format!("bat-pm-test-{:032x}", rand::random::<u128>()));
            private_dir(&root).unwrap();
            let events = Arc::new(Mutex::new(vec![]));
            let captured = events.clone();
            let mut host = crate::host_context::HeadlessHost::new(
                Some(root.clone()),
                Arc::new(move |topic, payload| {
                    captured
                        .lock()
                        .unwrap()
                        .push((topic.into(), payload.clone()))
                }),
            );
            host.manage(crate::event_hub::RuntimeEventHubState::default());
            host.manage(crate::commands::notification::AgentNotificationState::default());
            let app = HostContext::from_headless(Arc::new(host));
            let state = CodexAppServerState::default();
            let mut opts = options();
            opts["pmTimeoutMs"] = json!(timeout);
            let pm =
                PmState::create(&app, "pm-session", &opts, Duration::from_millis(timeout)).unwrap();
            state.inner.sessions.lock().unwrap().insert(
                "pm-session".into(),
                pm_session("pm-session", "/logical/repository", "test-model"),
            );
            state.inner.session_runtimes.lock().unwrap().insert(
                "pm-session".into(),
                Arc::new(CodexRuntime {
                    pm: Some(pm.clone()),
                    ..Default::default()
                }),
            );
            let config = root.join("config.json");
            fs::write(&config, serde_json::to_vec(&nested_config()).unwrap()).unwrap();
            let script = root.join("fake.py");
            fs::write(&script, FAKE).unwrap();
            let mut command = Command::new("python3");
            command
                .args(["-u"])
                .arg(script)
                .arg(mode)
                .arg(config)
                .arg(root.join("calls.ndjson"))
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            (
                Self {
                    app,
                    state,
                    pm,
                    root,
                    events,
                },
                command,
            )
        }
        fn send(&self) -> Result<Value, BridgeError> {
            self.state.send_pm_message(
                &self.app,
                "pm-session",
                " frozen context \n",
                &[],
                &json!({"clientMessageId":"original-client-1",
                "pmRuntimeId":self.pm.view()["runtimeId"],"pmContextDigest":"a".repeat(64)}),
            )
        }
        fn settled(&self) -> Value {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                let record = self.pm.view();
                if record["exit"]["confirmedDead"] == true {
                    return record;
                }
                assert!(Instant::now() < until, "runtime did not settle: {record}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        fn calls(&self) -> Vec<Value> {
            fs::read_to_string(self.root.join("calls.ndjson"))
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            self.state.stop_pm_session("pm-session", "test_cleanup");
            std::thread::sleep(Duration::from_millis(30));
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    const FAKE: &str = r#"
import json,sys,time
mode,config_path,calls_path=sys.argv[1:]
config=json.load(open(config_path))
if mode=='bad-config': config['features']['shell_tool']=True
def send(v): print(json.dumps(v),flush=True)
def event(method,params): send({'method':method,'params':params})
for line in sys.stdin:
 q=json.loads(line)
 with open(calls_path,'a') as f: f.write(json.dumps(q)+'\n')
 method=q['method']; p=q.get('params',{}); result={}
 if method=='initialized': continue
 if method=='config/read': result={'config':config}
 if method=='skills/list': result={'data':[{'skills':[],'errors':[]}]}
 if method=='thread/start':
  result={'thread':{'id':'thread-original','environments':[],'turns':[],'ephemeral':True},'sandbox':{'type':'readOnly','networkAccess':False},'approvalPolicy':'never','instructionSources':[],'cwd':p['cwd'],'model':p['model']}
 if method=='mcpServerStatus/list': result={'data':[],'nextCursor':None}
 if method=='turn/start':
  if mode=='lost': time.sleep(10);continue
  if mode=='oversized': print('x'*1100000,flush=True);time.sleep(10);continue
  send({'id':q['id'],'result':{'turn':{'id':'turn-original'}}})
  event('turn/started',{'threadId':'thread-original','turn':{'id':'turn-original','status':'inProgress'}})
  if mode=='tool': event('item/started',{'threadId':'thread-original','turnId':'turn-original','item':{'id':'bad','type':'commandExecution','command':'forbidden'}});time.sleep(10);continue
  if mode=='request': send({'id':99,'method':'item/commandExecution/requestApproval','params':{'threadId':'thread-original'}});time.sleep(10);continue
  if mode=='hold': time.sleep(10);continue
  user={'id':'user-original','type':'userMessage','clientId':p['clientUserMessageId'],'content':p['input']}
  if mode=='wrong-input': user['content']=[{'type':'text','text':'substituted'}]
  agent={'id':'assistant-original','type':'agentMessage','text':'Grounded PM answer.','phase':'final_answer'}
  for item in ([agent] if mode=='missing-input' else [user,agent]):
   event('item/started',{'threadId':'thread-original','turnId':'turn-original','item':item})
   event('item/completed',{'threadId':'thread-original','turnId':'turn-original','item':item})
  if mode=='wrong-summary': agent=dict(agent,text='Unobserved replacement')
  event('turn/completed',{'threadId':'thread-original','turn':{'id':'turn-original','status':'completed','itemsView':'summary','items':[agent]}})
  time.sleep(10);continue
 send({'id':q['id'],'result':result})
 if mode=='blocked-pipe' and method=='mcpServerStatus/list': time.sleep(10)
"#;
    fn wait_for(rig: &Rig, predicate: impl Fn(&Value) -> bool) {
        let until = Instant::now() + Duration::from_secs(2);
        while !predicate(&rig.pm.view()) {
            assert!(
                Instant::now() < until,
                "timed out waiting for runtime state"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn pm_stop_before_spawn_gate_prevents_later_launch() {
        let (rig, command) = Rig::unstarted("happy", 5000);
        let arrived = Arc::new(std::sync::Barrier::new(2));
        let proceed = Arc::new(std::sync::Barrier::new(2));
        *rig.pm.before_launch.lock().unwrap() = Some((arrived.clone(), proceed.clone()));
        std::thread::scope(|scope| {
            let running = scope.spawn(|| {
                rig.state.for_session("pm-session").run_pm_command(
                    &rig.app,
                    "pm-session",
                    "test-model",
                    &rig.pm,
                    command,
                    None,
                )
            });
            arrived.wait();
            let stopped = rig
                .state
                .stop_pm_session("pm-session", "test_stop_before_spawn");
            assert!(rig.pm.stop_requested.load(Ordering::Acquire));
            assert_eq!(stopped["pmRuntime"]["launchAttempted"], false);
            assert!(stopped["pmRuntime"]["process"].is_null());
            proceed.wait();
            assert_eq!(running.join().unwrap().unwrap_err(), "PM_RUNTIME_STOPPED");
        });
        assert_eq!(rig.pm.view()["launchAttempted"], false);
        assert!(rig.calls().is_empty());
    }
    #[test]
    fn pm_stop_while_send_waits_for_stdin_prevents_the_original_frame() {
        let rig = Rig::new("happy", 5000);
        let connection = rig
            .state
            .for_session("pm-session")
            .live_connection()
            .unwrap();
        let stdin = connection.stdin.lock().unwrap();
        std::thread::scope(|scope| {
            let sending = scope.spawn(|| rig.send());
            wait_for(&rig, |r| r["send"]["attempted"] == true);
            let stopped = rig
                .state
                .stop_pm_session("pm-session", "test_stop_before_frame");
            assert_eq!(stopped["pmRuntime"]["exit"]["confirmedDead"], true);
            drop(stdin);
            assert!(sending.join().unwrap().is_err());
        });
        assert!(!rig.calls().iter().any(|q| q["method"] == "turn/start"));
        assert!(rig.send().is_err());
        let retained = CodexAppServerState::default()
            .get_pm_runtime(&rig.app, "pm-session")
            .unwrap();
        assert_eq!(retained["pmRuntime"]["send"]["attempted"], true);
        assert!(retained["pmRuntime"]["terminal"].is_null());
    }
    #[test]
    fn pm_stop_interrupts_partial_nonblocking_frame_without_releasing_send_slot() {
        let rig = Rig::new("blocked-pipe", 5000);
        let connection = rig
            .state
            .for_session("pm-session")
            .live_connection()
            .unwrap();
        std::thread::scope(|scope| {
            let sending = scope.spawn(|| rig.state.send_pm_message(&rig.app, "pm-session", &"x".repeat(LINE_LIMIT/2), &[],
                &json!({"clientMessageId":"original-client-1","pmRuntimeId":rig.pm.view()["runtimeId"],"pmContextDigest":"a".repeat(64)})));
            wait_for(&rig, |r| r["send"]["attempted"] == true);
            // Once the pipe is full there is an actual partial, unread frame;
            // wait until its writer owns the gate instead of relying on a sleep.
            let until = Instant::now() + Duration::from_secs(2);
            loop {
                // The writer owns stdin, so inspect the actual pipe through
                // /proc rather than acquiring that mutex or guessing timing.
                let fd_path = format!("/proc/{}/fd/0", connection.pid);
                let pipe = File::open(fd_path).unwrap();
                use std::os::fd::AsRawFd;
                let mut unread: libc::c_int = 0;
                assert_eq!(
                    unsafe { libc::ioctl(pipe.as_raw_fd(), libc::FIONREAD, &mut unread) },
                    0
                );
                if unread > 0 && rig.pm.effect_gate.try_lock().is_err() {
                    break;
                }
                assert!(Instant::now() < until);
                std::thread::yield_now();
            }
            let started = Instant::now();
            let stopped = rig.state.stop_pm_session("pm-session", "test_blocked_pipe");
            assert!(started.elapsed() < Duration::from_secs(2));
            assert_eq!(stopped["pmRuntime"]["exit"]["confirmedDead"], true);
            assert!(sending.join().unwrap().is_err());
        });
        assert_eq!(
            connection.next_id.load(Ordering::Acquire),
            rig.pm.view()["send"]["requestId"].as_u64().unwrap()
        );
        assert!(!rig.calls().iter().any(|q| q["method"] == "turn/start"));
        assert!(rig.send().is_err());
        let retained = CodexAppServerState::default()
            .get_pm_runtime(&rig.app, "pm-session")
            .unwrap();
        assert_eq!(retained["pmRuntime"]["send"]["attempted"], true);
        assert!(retained["pmRuntime"]["send"]["acknowledged"] == false);
        assert!(retained["pmRuntime"]["terminal"].is_null());
    }
    #[test]
    fn pm_options_require_explicit_profile_and_frozen_carriers() {
        let mut value = options();
        assert!(checked_options(&value).is_ok());
        for key in [
            "prompt",
            "resumeSessionId",
            "mcpServers",
            "dynamicTools",
            "sandboxPolicy",
        ] {
            let mut v = value.clone();
            v[key] = json!("injected");
            assert!(checked_options(&v).is_err());
        }
        value["pmTimeoutMs"] = json!(true);
        assert!(checked_options(&value).is_err());
        value = options();
        value["codexSandboxMode"] = json!("workspace-write");
        assert!(checked_options(&value).is_err());
        value = options();
        value["agentPreset"] = json!("claude-worktree");
        assert!(checked_options(&value).is_err());
        assert!(should_handle_codex(&Some(value))); // Never falls through to Node.
    }
    #[test]
    fn pm_config_requires_every_observable_restriction() {
        let config = nested_config();
        validate_config(&config).unwrap();
        for (key, _) in settings(Path::new("/unused")) {
            if key.starts_with("tools.") || key == "log_dir" || key == "sqlite_home" {
                continue;
            }
            let mut changed = config.clone();
            let parts: Vec<_> = key.split('.').collect();
            let mut current = &mut changed;
            for part in &parts[..parts.len() - 1] {
                current = &mut current[*part];
            }
            current
                .as_object_mut()
                .unwrap()
                .remove(parts[parts.len() - 1]);
            assert!(validate_config(&changed).is_err(), "{key}");
        }
        let mut changed = config;
        changed["mcp_servers"] = json!({"injected":{}});
        assert!(validate_config(&changed).is_err());
    }
    #[test]
    fn pm_original_turn_and_process_exit_keep_native_evidence() {
        let rig = Rig::new("happy", 3000);
        let original = rig.pm.view();
        assert_eq!(original["attestation"]["freshThread"], true);
        assert_eq!(
            rig.state.get_session_meta("pm-session").unwrap()["numTurns"],
            0
        );
        assert!(
            rig.state.get_session_state("pm-session").unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        rig.send().unwrap();
        let record = rig.settled();
        assert_eq!(record["terminal"]["native"], true);
        assert_eq!(record["terminal"]["turnId"], "turn-original");
        assert_eq!(record["send"]["promptDigest"], digest(" frozen context \n"));
        assert_eq!(
            record["send"]["observedClientMessageId"],
            "original-client-1"
        );
        assert_eq!(record["exit"]["process"], original["process"]);
        assert_eq!(record["exit"]["action"], "native_terminal");
        assert!(rig.send().is_err());
        assert_eq!(
            rig.calls()
                .iter()
                .filter(|q| q["method"] == "turn/start")
                .count(),
            1
        );
        let events = rig.events.lock().unwrap();
        assert!(events
            .iter()
            .filter(|(name, _)| name == "claude:status")
            .all(|(_, p)| p["payload"].get("pmRuntime").is_none()));
        assert!(events.iter().any(
            |(name, p)| name == "claude:turn-end" && p["payload"]["turnId"] == "turn-original"
        ));
        assert!(!events
            .iter()
            .any(|(name, p)| name == "claude:turn-end" && p["payload"]["reason"] == "aborted"));
    }
    #[test]
    fn pm_lost_send_response_never_recreates_thread_or_resends() {
        let rig = Rig::new("lost", 600);
        assert!(rig.send().is_err());
        let record = rig.settled();
        assert_eq!(record["send"]["attempted"], true);
        assert!(record["terminal"].is_null());
        assert!(rig.send().is_err());
        assert_eq!(
            rig.calls()
                .iter()
                .filter(|q| q["method"] == "turn/start")
                .count(),
            1
        );
        assert_eq!(
            rig.calls()
                .iter()
                .filter(|q| q["method"] == "thread/start")
                .count(),
            1
        );
    }
    #[test]
    fn pm_ack_and_stop_do_not_fabricate_native_completion() {
        let rig = Rig::new("hold", 3000);
        rig.send().unwrap();
        let stopped = rig
            .state
            .abort_session(&rig.app, "pm-session".into())
            .unwrap();
        assert_eq!(stopped["pmRuntime"]["exit"]["confirmedDead"], true);
        assert!(stopped["pmRuntime"]["terminal"].is_null());
        assert_eq!(stopped["pmRuntime"]["send"]["acknowledged"], true);
        assert!(!rig
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|(name, _)| name == "claude:turn-end"));
    }
    #[test]
    fn pm_config_failure_never_sends_a_model_input() {
        let rig = Rig::new("bad-config", 3000);
        assert!(rig.send().is_err());
        let retained = CodexAppServerState::default()
            .get_pm_runtime(&rig.app, "pm-session")
            .unwrap();
        assert!(retained["pmRuntime"]["attestation"].is_null());
        assert!(retained["pmRuntime"]["process"].is_object());
        assert_eq!(retained["pmRuntime"]["exit"]["confirmedDead"], true);
        assert!(!rig
            .calls()
            .iter()
            .any(|q| q["method"] == "turn/start" || q["method"] == "thread/start"));
    }
    #[test]
    fn pm_unexpected_native_tools_requests_and_input_fail_closed() {
        for mode in [
            "tool",
            "request",
            "wrong-input",
            "oversized",
            "missing-input",
            "wrong-summary",
        ] {
            let rig = Rig::new(mode, 3000);
            let _ = rig.send();
            let record = rig.settled();
            assert!(record["terminal"].is_null(), "{mode}");
            assert!(!rig
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|(name, _)| name == "claude:permission-request" || name == "claude:turn-end"));
            assert!(rig.send().is_err());
        }
    }
    #[test]
    fn pm_readback_survives_restart_without_taking_over_the_process() {
        let rig = Rig::new("happy", 3000);
        rig.send().unwrap();
        let old = rig.settled();
        let restarted = CodexAppServerState::default();
        let read = restarted.get_pm_runtime(&rig.app, "pm-session").unwrap();
        for field in [
            "runtimeId",
            "process",
            "threadId",
            "send",
            "terminal",
            "exit",
        ] {
            assert_eq!(read["pmRuntime"][field], old[field]);
        }
        assert_eq!(read["pmRuntime"]["processObservation"]["state"], "dead");
        assert!(restarted
            .start_session(&rig.app, "pm-session".into(), Some(options()))
            .is_err());
        assert!(restarted
            .resume_session(
                &rig.app,
                "pm-session".into(),
                "thread-original".into(),
                Some(options())
            )
            .is_err());
        assert!(restarted.get_pm_runtime(&rig.app, "missing").is_err());
        assert_eq!(inspect_process(&Value::Null)["state"], "unknown");
    }
    #[test]
    fn pm_ordinary_session_and_shared_connection_are_not_mutated() {
        let rig = Rig::new("hold", 3000);
        rig.state.inner.sessions.lock().unwrap().insert(
            "manual".into(),
            pm_session("manual", "/human", "manual-model"),
        );
        let mut before = rig.state.get_session_state("manual").unwrap();
        before["meta"].as_object_mut().unwrap().remove("durationMs");
        rig.send().unwrap();
        rig.state.stop_session("pm-session".into());
        let mut after = rig.state.get_session_state("manual").unwrap();
        after["meta"].as_object_mut().unwrap().remove("durationMs");
        assert_eq!(after, before);
        assert!(rig.state.inner.connection.lock().unwrap().is_none());
        assert!(rig
            .state
            .reset_session(&rig.app, "pm-session".into())
            .is_err());
        assert!(rig.state.reload_session(&rig.app, "pm-session").is_err());
        assert_eq!(
            rig.state
                .set_model(&rig.app, "pm-session", "other".into())
                .unwrap()["ok"],
            false
        );
    }
    #[test]
    fn pm_new_send_requires_exact_original_runtime_context_and_client_id() {
        let rig = Rig::new("hold", 3000);
        for params in [
            json!({}),
            json!({"clientMessageId":"c","pmRuntimeId":"other","pmContextDigest":"a".repeat(64)}),
            json!({"clientMessageId":"c","pmRuntimeId":rig.pm.view()["runtimeId"],"pmContextDigest":"b".repeat(64)}),
        ] {
            assert!(rig
                .state
                .send_pm_message(&rig.app, "pm-session", "text", &[], &params)
                .is_err());
        }
        assert!(rig.pm.view()["send"].is_null());
        assert!(!rig.calls().iter().any(|q| q["method"] == "turn/start"));
    }
    #[test]
    fn pm_deadline_is_from_start_not_reset_at_send() {
        let rig = Rig::new("hold", 400);
        let record = rig.settled();
        assert_eq!(record["exit"]["action"], "deadline");
        assert!(rig.send().is_err());
        assert!(rig.pm.view()["send"].is_null());
    }
    #[test]
    fn pm_offline_bwrap_cannot_write_selected_checkout_or_receipts() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let rig = Rig::new("hold", 3000);
        rig.state.stop_pm_session("pm-session", "canary_setup");
        let host = rig.root.join("host");
        private_dir(&host).unwrap();
        let native = host.join(".codex");
        private_dir(&native).unwrap();
        fs::write(native.join("auth.json"), "synthetic-no-provider-auth").unwrap();
        let human = rig.root.join("human.txt");
        fs::write(&human, "original bytes").unwrap();
        let executable = rig.root.join("canary.sh");
        fs::write(&executable,format!("#!/bin/sh\nif printf changed > '{}' 2>/dev/null; then exit 20; fi\nif printf changed > '{}' 2>/dev/null; then exit 21; fi\nprintf owned > '{}'\n",human.display(),rig.pm.directory.join("receipt.json").display(),rig.pm.directory.join("workspace/canary").display())).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let auth = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(native.join("auth.json"))
            .unwrap();
        let mut command = sandbox_command(
            &rig.pm,
            &executable,
            &resolve_program("bwrap").unwrap(),
            &host,
            &native,
            &auth,
            true,
        )
        .unwrap();
        let mut child = command.spawn().unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= until {
                let _ = child.kill();
                panic!("canary timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(status.success(), "offline bwrap canary: {stderr}");
        assert_eq!(fs::read_to_string(human).unwrap(), "original bytes");
        assert_eq!(
            fs::read_to_string(rig.pm.directory.join("workspace/canary")).unwrap(),
            "owned"
        );
        assert_eq!(
            read_record(&rig.pm.directory.join("receipt.json"), "pm-session").unwrap()["runtimeId"],
            rig.pm.view()["runtimeId"]
        );
    }
    #[test]
    fn pm_restart_cannot_claim_death_or_replace_an_original_live_process() {
        let rig = Rig::new("hold", 3000);
        let restarted = CodexAppServerState::default();
        let observed = restarted.get_pm_runtime(&rig.app, "pm-session").unwrap();
        assert_eq!(
            observed["pmRuntime"]["processObservation"]["state"],
            "alive"
        );
        assert!(observed["pmRuntime"]["exit"].is_null());
        assert!(restarted
            .start_session(&rig.app, "pm-session".into(), Some(options()))
            .is_err());
        assert!(!rig
            .calls()
            .iter()
            .any(|request| request["method"] == "turn/start"));
        assert_eq!(rig.pm.view()["process"], observed["pmRuntime"]["process"]);
    }
}
