//! Bounded transcript reads. Cache only line offsets, never transcript bodies.
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::SystemTime,
};
#[cfg(test)]
thread_local! { static BYTES_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[derive(Clone)]
struct Index {
    size: u64,
    modified: Option<SystemTime>,
    rows: Arc<Vec<(u64, u64)>>,
}
static INDEX: OnceLock<Mutex<HashMap<PathBuf, Index>>> = OnceLock::new();
fn index(path: &Path) -> io::Result<Index> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    let cache = INDEX.get_or_init(Default::default);
    if let Some(hit) = cache
        .lock()
        .unwrap()
        .get(path)
        .filter(|v| v.size == meta.len() && v.modified == meta.modified().ok())
    {
        return Ok(hit.clone());
    }
    let mut rows = Vec::new();
    let mut buf = [0u8; 65536];
    let (mut position, mut start, mut nonempty) = (0u64, 0u64, false);
    loop {
        let remaining = meta.len().saturating_sub(position) as usize;
        if remaining == 0 {
            break;
        }
        let capacity = buf.len().min(remaining);
        let n = file.read(&mut buf[..capacity])?;
        #[cfg(test)]
        BYTES_READ.with(|v| v.set(v.get() + n));
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\n' {
                if nonempty {
                    rows.push((start, position));
                }
                start = position + 1;
                nonempty = false;
            } else if !b.is_ascii_whitespace() {
                nonempty = true;
            }
            position += 1;
        }
    }
    if nonempty {
        rows.push((start, position));
    }
    let result = Index {
        size: meta.len(),
        modified: meta.modified().ok(),
        rows: Arc::new(rows),
    };
    let mut cache = cache.lock().unwrap();
    if cache.len() >= 32 {
        cache.clear();
    }
    cache.insert(path.to_owned(), result.clone());
    Ok(result)
}
/// Offset/count archive contract; only selected rows are allocated/read after indexing.
pub(crate) fn archive_page(
    path: &Path,
    offset: usize,
    limit: usize,
) -> io::Result<(Vec<String>, usize, bool)> {
    let idx = index(path)?;
    let total = idx.rows.len();
    let end = total.saturating_sub(offset);
    let start = end.saturating_sub(limit);
    let mut file = File::open(path)?;
    let mut lines = Vec::new();
    let mut budget = 1800 * 1024usize;
    let mut selected_start = end;
    for row in (start..end).rev() {
        let (a, b) = idx.rows[row];
        if (b - a) as usize > budget {
            if !lines.is_empty() {
                break;
            }
            lines.push(serde_json::json!({ "id": format!("archive-gap-{a}-{b}"), "role": "system", "content": "[History gap: record exceeds page budget]", "historyGap": { "sizeBytes": b-a } }).to_string());
            selected_start = row;
            break;
        }
        budget -= (b - a) as usize;
        file.seek(SeekFrom::Start(a))?;
        let mut bytes = vec![0; (b - a) as usize];
        file.read_exact(&mut bytes)?;
        #[cfg(test)]
        BYTES_READ.with(|v| v.set(v.get() + bytes.len()));
        lines.push(String::from_utf8_lossy(&bytes).into_owned());
        selected_start = row;
    }
    lines.reverse();
    Ok((lines, total, selected_start > 0))
}
/// Exclusive byte cursor, reading backward from EOF in a fixed 2 MiB window.
pub(crate) fn tail(path: &Path, before: Option<u64>) -> io::Result<(String, Option<u64>, usize)> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let end = before.unwrap_or(size);
    if end > size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid history cursor",
        ));
    }
    let start = end.saturating_sub(2 * 1024 * 1024);
    file.seek(SeekFrom::Start(start.saturating_sub(1)))?;
    let mut bytes = vec![0; (end - start + u64::from(start > 0)) as usize];
    file.read_exact(&mut bytes)?;
    let mut first = if start == 0 {
        0
    } else if bytes[0] == b'\n' {
        1
    } else {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(bytes.len())
    };
    // Bound parser/item overhead as well as raw bytes.
    let starts: Vec<usize> = bytes[first..]
        .iter()
        .enumerate()
        .filter_map(|(i, b)| (*b == b'\n' && first + i + 1 < bytes.len()).then_some(first + i + 1))
        .collect();
    if starts.len() >= 500 {
        first = starts[starts.len() - 500];
    }
    let cursor = if first == bytes.len() {
        start
    } else {
        start.saturating_sub(u64::from(start > 0)) + first as u64
    };
    Ok((
        String::from_utf8_lossy(&bytes[first..]).into_owned(),
        (cursor > 0).then_some(cursor),
        bytes.len(),
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    pub(crate) fn large_fixture(path: &Path, mb: usize, codex: bool) -> usize {
        let mut file = io::BufWriter::new(File::create(path).unwrap());
        let padding = "x".repeat(760 * 1024);
        let mut bytes = 0;
        let mut rows = 0;
        while bytes < mb * 1024 * 1024 {
            let value = if codex {
                serde_json::json!({ "type":"event_msg", "payload": { "type":"user_message", "message":padding }, "timestamp":"2026-01-01T00:00:00Z" })
            } else {
                serde_json::json!({ "id":format!("row-{rows}"), "role":"user", "content":padding })
            };
            let line = value.to_string() + "\n";
            file.write_all(line.as_bytes()).unwrap();
            bytes += line.len();
            rows += 1;
        }
        rows
    }
    fn large_transcripts(mb: usize) {
        let dir = std::env::temp_dir().join(format!("bat-tail-{}-{mb}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("archive.jsonl");
        let total = large_fixture(&archive, mb, false);
        let (recent, count, more) = archive_page(&archive, 0, 120).unwrap();
        assert_eq!(count, total);
        assert!(more);
        assert_eq!(recent.len(), 2);
        assert!(recent.iter().map(String::len).sum::<usize>() < 2 * 1024 * 1024);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(recent.last().unwrap()).unwrap()["id"],
            format!("row-{}", total - 1)
        );
        // The index is retained without retaining any message text.
        let cached = index(&archive).unwrap();
        BYTES_READ.with(|v| v.set(0));
        let (older, _, _) = archive_page(&archive, recent.len(), 120).unwrap();
        assert!(
            BYTES_READ.with(|v| v.get()) < 2 * 1024 * 1024,
            "warm archive pages must not scan the transcript"
        );
        let messages: Vec<serde_json::Value> = older
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let response = serde_json::json!({"type":"invoke-result", "id":1, "result":{"messages":messages,"total":total,"hasMore":true}});
        assert!(
            serde_json::to_vec(&response).unwrap().len()
                < crate::remote_core::REMOTE_RESPONSE_MAX_BYTES
        );
        assert!(Arc::ptr_eq(&cached.rows, &index(&archive).unwrap().rows));
        assert_eq!(older.len(), 2);
        assert_ne!(older.last(), recent.first());
        let rollout = dir.join("rollout.jsonl");
        large_fixture(&rollout, mb, true);
        let (raw, before, bytes) = tail(&rollout, None).unwrap();
        assert!(bytes <= 2 * 1024 * 1024 + 1);
        assert!(before.is_some());
        assert_eq!(raw.lines().count(), 2);
        let (_, previous, bytes) = tail(&rollout, before).unwrap();
        assert!(previous < before);
        assert!(bytes <= 2 * 1024 * 1024 + 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn transcripts_30mb() {
        large_transcripts(30);
    }
    #[test]
    #[ignore = "generates 1GB of temporary transcripts; run locally"]
    fn transcripts_500mb() {
        large_transcripts(500);
    }
    #[test]
    fn archive_contract_and_invalidation() {
        let path = std::env::temp_dir().join(format!("bat-index-{}.jsonl", std::process::id()));
        std::fs::write(&path, "\n {}\n\r\n{\"id\":1}\n{\"id\":2}").unwrap();
        let (page, total, more) = archive_page(&path, 0, 2).unwrap();
        assert_eq!((page.len(), total, more), (2, 3, true));
        assert_eq!(archive_page(&path, 99, 2).unwrap(), (vec![], 3, false));
        std::fs::write(&path, "{\"id\":3}\n").unwrap();
        assert_eq!(archive_page(&path, 0, 2).unwrap().1, 1);
        std::fs::remove_file(path).unwrap();
    }
}
