//! Best-effort fleet error spool. Callers only try a bounded queue; one worker
//! owns all disk I/O. A full queue drops records rather than stalling the host.
mod redaction;
pub use redaction::{fingerprint, normalize, redact};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{mpsc, OnceLock},
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_BYTES: u64 = 10 * 1024 * 1024;
static SENDER: OnceLock<mpsc::SyncSender<(&'static str, &'static str, String)>> = OnceLock::new();

pub fn log(level: &'static str, code: &'static str, msg: &str) {
    if let Some(sender) = SENDER.get() {
        // Drop oversized input rather than truncating a credential mid-pattern.
        if msg.len() > 16384 {
            return;
        }
        let _ = sender.try_send((level, code, msg.to_owned()));
    }
}

pub fn default_path() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("BAT/logs/errors.jsonl"))
    } else {
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".bat/logs/errors.jsonl"))
    }
}

/// Install only in headless server mode. Rust panics (including panicking async
/// tasks) are the runtime equivalent of uncaught exceptions/rejections.
/// Fatal exits may lose queued records: this logger never waits for disk.
pub fn install(version: &'static str) {
    let Some(path) = default_path() else { return };
    if SENDER.get().is_some() {
        return;
    }
    let (tx, rx) = mpsc::sync_channel::<(&'static str, &'static str, String)>(256);
    if std::thread::Builder::new()
        .name("bat-error-log".into())
        .spawn(move || {
            let machine = std::env::var("HOSTNAME")
                .or_else(|_| std::env::var("COMPUTERNAME"))
                .ok()
                .or_else(|| {
                    std::process::Command::new("hostname")
                        .output()
                        .ok()
                        .filter(|out| out.status.success())
                        .and_then(|out| String::from_utf8(out.stdout).ok())
                        .map(|name| name.trim().to_owned())
                })
                .unwrap_or_else(|| "unknown".into());
            let mut spool = Spool::new(path, redact(&machine), version.into(), MAX_BYTES);
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok((level, code, msg)) => {
                        spool.record(level, code, &msg, Instant::now());
                        // These paths can terminate the process immediately. Give the
                        // worker a chance to persist without waiting for aggregation.
                        if matches!(code, "server.uncaught" | "server.start_failed") {
                            spool.flush(Instant::now(), true);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        spool.flush(Instant::now(), true);
                        break;
                    }
                }
                spool.flush(Instant::now(), false);
            }
        })
        .is_err()
    {
        return;
    }
    if SENDER.set(tx).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("Rust panic");
        log("error", "server.uncaught", msg);
        previous(info);
    }));
}

struct Spool {
    path: PathBuf,
    machine: String,
    version: String,
    max_bytes: u64,
    pending: HashMap<String, (Instant, Value)>,
}
impl Spool {
    fn new(path: PathBuf, machine: String, version: String, max_bytes: u64) -> Self {
        Self {
            path,
            machine,
            version,
            max_bytes,
            pending: HashMap::new(),
        }
    }
    fn record(&mut self, level: &str, code: &str, msg: &str, now: Instant) {
        self.flush(now, false);
        let msg = redact(msg);
        let fp = fingerprint("bat-server", code, &msg);
        if let Some((_, record)) = self.pending.get_mut(&fp) {
            record["count"] = json!(record["count"].as_u64().unwrap_or(1).saturating_add(1));
            return;
        }
        // Bound distinct fingerprints even during an error storm.
        if self.pending.len() >= 256 {
            self.flush(now, true);
        }
        self.pending.insert(
            fp.clone(),
            (
                now,
                json!({
                    "ts": chrono::Utc::now().to_rfc3339(), "machine_id": self.machine,
                    "component": "bat-server", "version": self.version, "level": level,
                    "code": code, "msg": msg, "fingerprint": fp, "count": 1, "context": {}
                }),
            ),
        );
    }
    fn flush(&mut self, now: Instant, all: bool) {
        self.pending.retain(|_, (first, record)| {
            if !all && now.duration_since(*first) < WINDOW {
                return true;
            }
            if let Ok(mut line) = serde_json::to_vec(record) {
                line.push(b'\n');
                let _ = append(&self.path, &line, self.max_bytes);
            }
            false
        });
    }
}
fn append(path: &Path, line: &[u8], limit: u64) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::metadata(path).map(|m| m.len()).unwrap_or(0) + line.len() as u64 > limit {
        let _ = fs::remove_file(path.with_extension("jsonl.4"));
        for n in (1..4).rev() {
            let src = path.with_extension(format!("jsonl.{n}"));
            if src.exists() {
                fs::rename(src, path.with_extension(format!("jsonl.{}", n + 1)))?;
            }
        }
        if path.exists() {
            fs::rename(path, path.with_extension("jsonl.1"))?;
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connector_corpus() {
        let fixture: Value = serde_json::from_str(include_str!("../tests/fixture.json")).unwrap();
        for case in fixture["redaction_cases"].as_array().unwrap() {
            assert!(
                redact(case["msg"].as_str().unwrap()) == case["expected"].as_str().unwrap(),
                "redaction fixture mismatch"
            );
        }
        for secret in fixture["secrets"].as_array().unwrap() {
            assert!(
                redact(secret.as_str().unwrap()).contains("[REDACTED]"),
                "secret shape missed"
            );
        }
        for case in fixture["fingerprints"].as_array().unwrap() {
            let msg = case["msg"].as_str().unwrap();
            assert_eq!(normalize(msg), case["normalized"].as_str().unwrap());
            assert_eq!(
                fingerprint(
                    case["component"].as_str().unwrap(),
                    case["code"].as_str().unwrap(),
                    msg
                ),
                case["fingerprint"].as_str().unwrap()
            );
        }
    }
    #[test]
    fn disk_records_are_redacted_and_windows_expire() {
        let dir = std::env::temp_dir().join(format!("bat-errlog-redaction-{}", std::process::id()));
        let path = dir.join("errors.jsonl");
        let mut spool = Spool::new(path.clone(), "test".into(), "1.2.3".into(), MAX_BYTES);
        let fixture: Value = serde_json::from_str(include_str!("../tests/fixture.json")).unwrap();
        let now = Instant::now();
        for case in fixture["redaction_cases"].as_array().unwrap() {
            spool.record(
                "error",
                "test.redaction",
                case["msg"].as_str().unwrap(),
                now,
            );
        }
        spool.flush(now, true);
        for line in fs::read_to_string(&path).unwrap().lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            assert_eq!(record["msg"], redact(record["msg"].as_str().unwrap()));
            assert_eq!(
                record["fingerprint"],
                fingerprint(
                    "bat-server",
                    "test.redaction",
                    record["msg"].as_str().unwrap()
                )
            );
            assert!(chrono::DateTime::parse_from_rfc3339(record["ts"].as_str().unwrap()).is_ok());
        }
        spool.record("warn", "test.window", "failure 1", now);
        spool.record("warn", "test.window", "failure 2", now + WINDOW);
        assert_eq!(spool.pending.values().next().unwrap().1["count"], 1);
        spool.flush(now + WINDOW, true);
        let records = fs::read_to_string(&path).unwrap();
        assert_eq!(
            records
                .lines()
                .filter(|line| line.contains("test.window"))
                .count(),
            2
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn collapse_rotation_and_write_failure() {
        let dir = std::env::temp_dir().join(format!("bat-errlog-test-{}", std::process::id()));
        let path = dir.join("errors.jsonl");
        let mut s = Spool::new(path.clone(), "test".into(), "1.0.0".into(), 600);
        let now = Instant::now();
        s.record("error", "server.test", "failed 123", now);
        s.record(
            "error",
            "server.test",
            "failed 456",
            now + Duration::from_secs(59),
        );
        assert_eq!(s.pending.len(), 1);
        s.flush(now + WINDOW, false);
        let record: Value =
            serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(record["count"], 2);
        assert_eq!(record["component"], "bat-server");
        for _ in 0..8 {
            s.record("warn", "server.test", "failed 789", now);
            s.flush(now, true);
        }
        assert!(path.with_extension("jsonl.4").exists());
        assert!(!path.with_extension("jsonl.5").exists());
        let mut broken = Spool::new(path.join("impossible"), "test".into(), "1".into(), 600);
        broken.record("error", "server.test", "failure", now);
        broken.flush(now, true);
        assert!(broken.pending.is_empty());
        let _ = fs::remove_dir_all(dir);
    }
}
