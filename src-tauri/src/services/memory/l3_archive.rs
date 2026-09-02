//! L3 — Cold archive.
//!
//! Compressed (gzip) chunks of old L1 events. The on-disk format is
//! `<year>-<month>.jsonl.gz` (one file per month) so listing the
//! directory is enough to know what's archived. Each line inside the
//! gzip is one `MemoryEvent` JSON, same shape as the L1 `events.jsonl`.
//!
//! Why per-month: humans think in months, queries are usually
//! "everything from last March" or "last 90 days", and the
//! per-file-index for `gunzip`/`zcat` is cheap.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use super::schema::MemoryEvent;

/// L3 cold archive. Holds just a directory reference + a cached count
/// of the archived events (recomputed on demand; cheap because there
/// are at most a few dozen gzip files).
pub struct L3Archive {
    dir: PathBuf,
}

impl L3Archive {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append a batch of events to the appropriate monthly gzip chunk.
    /// Creates the file if it doesn't exist; otherwise appends a
    /// single new line to the existing gzip (gzip supports append for
    /// small additions — the trailing member gets slightly worse
    /// compression, which is fine for a once-per-month rollup).
    pub fn write_chunk(&self, events: &[MemoryEvent]) -> Result<String, super::MemoryError> {
        if events.is_empty() {
            return Ok(String::new());
        }
        // All events in a chunk should share a (year, month); we pick
        // from the first event's ts. The caller is responsible for
        // passing a coherent batch.
        let (year, month) = year_month(events[0].ts);
        let fname = format!("{:04}-{:02}.jsonl.gz", year, month);
        let path = self.dir.join(&fname);

        // For an existing file we have to read it, decompress, append,
        // and rewrite — `flate2::write::GzEncoder` doesn't support
        // append mode for non-empty streams. For a new file, the empty
        // case is the same shape (just no input).
        //
        // Heuristic: if the file is empty, write a fresh gzip. If not,
        // decompress / re-compress the whole thing.
        let existing: Vec<MemoryEvent> = if path.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
            self.read_chunk_by_path(&path)?
        } else {
            Vec::new()
        };

        let merged: Vec<MemoryEvent> = existing
            .into_iter()
            .chain(events.iter().cloned())
            .collect();

        let f = File::create(&path)
            .map_err(|e| super::MemoryError::Io(format!("recreate {fname}: {e}")))?;
        let mut enc = GzEncoder::new(f, Compression::default());
        for ev in &merged {
            let line = serde_json::to_string(ev)?;
            enc.write_all(line.as_bytes())?;
            enc.write_all(b"\n")?;
        }
        enc.finish()
            .map_err(|e| super::MemoryError::Io(format!("finish gzip: {e}")))?;
        Ok(fname)
    }

    /// Read all events back from one monthly chunk. Used during
    /// consolidation when the caller wants to inspect before merging.
    pub fn read_chunk(&self, year: i32, month: u32) -> Result<Vec<MemoryEvent>, super::MemoryError> {
        let fname = format!("{:04}-{:02}.jsonl.gz", year, month);
        let path = self.dir.join(&fname);
        if !path.exists() {
            return Ok(Vec::new());
        }
        self.read_chunk_by_path(&path)
    }

    fn read_chunk_by_path(&self, path: &Path) -> Result<Vec<MemoryEvent>, super::MemoryError> {
        let f = File::open(path)
            .map_err(|e| super::MemoryError::Io(format!("open {}: {e}", path.display())))?;
        let gz = GzDecoder::new(f);
        let r = BufReader::new(gz);
        let mut out = Vec::new();
        for (i, line) in r.lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<MemoryEvent>(&line) {
                Ok(ev) => out.push(ev),
                Err(e) => {
                    tracing::warn!(file = %path.display(), line = i, ?e, "l3: skipping unparseable line");
                }
            }
        }
        Ok(out)
    }

    /// Count of all events across all chunks. We do this by reading
    /// the directory and counting lines (line-count via
    /// `BufRead::lines` after gunzip is cheap for monthly files).
    pub fn count_cached(&self) -> u64 {
        self.count_real().unwrap_or(0)
    }

    fn count_real(&self) -> Result<u64, std::io::Error> {
        let mut total = 0u64;
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".jsonl.gz") {
                continue;
            }
            let f = File::open(entry.path())?;
            let gz = GzDecoder::new(f);
            let r = BufReader::new(gz);
            total += r.lines().count() as u64;
        }
        Ok(total)
    }

    /// List of chunk file names, e.g. `["2026-07.jsonl.gz", "2026-08.jsonl.gz"]`.
    pub fn list_chunks(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                if let Ok(ft) = entry.file_type() {
                    if ft.is_file() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.ends_with(".jsonl.gz") {
                            out.push(name);
                        }
                    }
                }
            }
        }
        out.sort();
        out
    }
}

/// Convert ms-since-epoch to (year, month) in UTC. Used to bucket
/// events into monthly chunks.
fn year_month(ts_ms: i64) -> (i32, u32) {
    let secs = ts_ms / 1000;
    let (y, m, _) = epoch_seconds_to_ymd(secs);
    (y, m)
}

/// Minimal `time` crate replacement — we only need Y/M/D from
/// `UnixEpoch + secs`. Avoids a transitive `time` dependency.
fn epoch_seconds_to_ymd(secs: i64) -> (i32, u32, u32) {
    // Howard Hinnant's date algorithm — public domain, see
    // http://howardhinnant.github.io/date_algorithms.html
    let z = secs / 86_400 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::memory::schema::EventKind;
    use std::env;

    fn temp_dir() -> PathBuf {
        let mut p = env::temp_dir();
        p.push(format!("luna-mem-l3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn ev_at(ts: i64, content: &str) -> MemoryEvent {
        MemoryEvent {
            id: uuid::Uuid::new_v4().to_string(),
            ts,
            kind: EventKind::ChatTurn,
            content: content.into(),
            payload: None,
            tags: vec![],
            source: "test".into(),
            importance: 0.5,
            secret: false,
        }
    }

    #[test]
    fn write_and_read_chunk_roundtrip() {
        let dir = temp_dir();
        let l3 = L3Archive::new(dir.clone());
        // Use the actual current year so the test is timezone-agnostic.
        // We pin to month 8 (August) of the current year.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let (year, _) = year_month(now);
        // Build a ts that lands in August of `year` (month 8):
        // pick a date 1 year ago to avoid leap-year corner cases.
        let ts = ms_for_y_m(year, 8, 15);
        let evs = vec![ev_at(ts, "a"), ev_at(ts + 1000, "b")];
        let fname = l3.write_chunk(&evs).unwrap();
        assert_eq!(fname, format!("{year}-08.jsonl.gz"));
        let back = l3.read_chunk(year, 8).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].content, "a");
        assert_eq!(back[1].content, "b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_chunk_appends_to_existing() {
        let dir = temp_dir();
        let l3 = L3Archive::new(dir.clone());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let (year, _) = year_month(now);
        let ts = ms_for_y_m(year, 8, 15);
        l3.write_chunk(&[ev_at(ts, "first")]).unwrap();
        l3.write_chunk(&[ev_at(ts + 1000, "second")]).unwrap();
        let back = l3.read_chunk(year, 8).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].content, "first");
        assert_eq!(back[1].content, "second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_chunks_returns_sorted_names() {
        let dir = temp_dir();
        let l3 = L3Archive::new(dir.clone());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let (year, _) = year_month(now);
        // Two different months of the same year.
        l3.write_chunk(&[ev_at(ms_for_y_m(year, 8, 15), "a")]).unwrap();
        l3.write_chunk(&[ev_at(ms_for_y_m(year, 6, 15), "b")]).unwrap();
        let names = l3.list_chunks();
        let want = vec![
            format!("{year}-06.jsonl.gz"),
            format!("{year}-08.jsonl.gz"),
        ];
        assert_eq!(names, want);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn year_month_basic() {
        let (y, m) = year_month(0);
        assert_eq!((y, m), (1970, 1));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let (y, m) = year_month(now);
        // We just want the roundtrip to be self-consistent.
        let (y2, m2) = year_month(ms_for_y_m(y, m, 15));
        assert_eq!((y, m), (y2, m2));
    }

    /// Build a Unix-ms timestamp for (year, month, day) at 00:00 UTC.
    /// Inverse of `year_month` for tests. We use the same algorithm.
    fn ms_for_y_m(y: i32, m: u32, d: u32) -> i64 {
        // Howard Hinnant's date algorithm, inverse direction.
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = (y - era * 400) as u64; // [0, 399]
        let doy = (153 * (m as i64 - if m > 2 { 3 } else { -9 }) + 2) / 5 + d as i64 - 1; // [0, 365]
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u64; // [0, 146096]
        let days = (era as i64) * 146_097 + (doe as i64) - 719_468;
        days * 86_400_000
    }
}
