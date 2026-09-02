//! L1 — Episodic memory.
//!
//! Append-only event log, two files on disk:
//! - `events.jsonl` — one JSON object per line, never edited in place.
//! - `index.sqlite` — small SQLite db for fast queries by `ts` / `kind`
//!   / `tags` / `id`. We use SQLite as an *index* (the source of truth
//!   is the JSONL), so the JSONL can always be replayed to rebuild the
//!   index if SQLite gets corrupted.
//!
//! Concurrency: appends are serialized via an internal `Mutex<File>`.
//! Reads don't touch the file at all (they hit SQLite only, which is
//! the whole point of having an index).
//!
//! Durability: we `flush()` after every write. Performance-wise, a
//! 4k-page Linux fsync on an SSD is ~1 ms; for our event rate (a
//! few per minute at peak) that's invisible.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use parking_lot::Mutex as PlMutex;
use rusqlite::{params, Connection};

use super::schema::{EventKind, MemoryEvent};

/// L1 — episodic memory.
pub struct L1Episodic {
    /// Append-only JSONL. The writer is wrapped in a `Mutex<File>` so
    /// concurrent `add_event` calls serialize at the file level.
    writer: Mutex<BufWriter<File>>,
    /// Path to the JSONL. Kept for `rebuild_index`.
    events_path: PathBuf,
    /// SQLite index.
    conn: PlMutex<Connection>,
}

impl L1Episodic {
    /// Open (or create) the JSONL + SQLite index pair. Idempotent.
    pub fn open(events_path: &Path, index_path: &Path) -> Result<Self, super::MemoryError> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(events_path)
            .map_err(|e| super::MemoryError::Io(format!("open events.jsonl: {e}")))?;
        let writer = Mutex::new(BufWriter::new(file));

        let conn = Connection::open(index_path)
            .map_err(|e| super::MemoryError::Io(format!("open index.sqlite: {e}")))?;
        // WAL mode: readers don't block the writer.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        Self::ensure_schema(&conn)?;

        // First-run: if events.jsonl exists but the index is empty,
        // rebuild it. This makes the system self-healing after a
        // crash that lost the index.
        let ev_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        if ev_count == 0 {
            drop(conn);
            Self::rebuild_index(events_path, index_path)?;
        }

        let conn = Connection::open(index_path)
            .map_err(|e| super::MemoryError::Io(format!("reopen index.sqlite: {e}")))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        Ok(Self {
            writer,
            events_path: events_path.to_path_buf(),
            conn: PlMutex::new(conn),
        })
    }

    fn ensure_schema(conn: &Connection) -> Result<(), super::MemoryError> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                id         TEXT PRIMARY KEY,
                ts         INTEGER NOT NULL,
                kind       TEXT NOT NULL,
                content    TEXT NOT NULL,
                payload    TEXT,
                tags       TEXT NOT NULL DEFAULT '',
                source     TEXT NOT NULL DEFAULT 'agent',
                importance REAL NOT NULL DEFAULT 0.5,
                secret     INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts DESC);
            CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
            CREATE INDEX IF NOT EXISTS idx_events_secret ON events(secret);

            -- Tag search via LIKE is enough for the volumes we expect
            -- (< 100k events). For larger volumes we'd add an FTS5
            -- virtual table; defer until M5 if needed.
            CREATE INDEX IF NOT EXISTS idx_events_tags ON events(tags);
            "#,
        )?;
        Ok(())
    }

    /// Append one event. Atomically writes the JSONL line + inserts
    /// the index row. The JSONL write is flushed before the SQLite
    /// insert, so if we crash mid-insert the worst case is an event
    /// in the JSONL with no index row — `rebuild_index` will fix it.
    pub fn append(&self, ev: &MemoryEvent) -> Result<String, super::MemoryError> {
        let line = serde_json::to_string(ev)?;
        {
            let mut w = self.writer.lock().expect("writer mutex poisoned");
            w.write_all(line.as_bytes())
                .and_then(|_| w.write_all(b"\n"))
                .map_err(|e| super::MemoryError::Io(format!("write events.jsonl: {e}")))?;
            w.flush().map_err(|e| super::MemoryError::Io(format!("flush: {e}")))?;
        }
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO events
             (id, ts, kind, content, payload, tags, source, importance, secret)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ev.id,
                ev.ts,
                ev.kind.as_str(),
                ev.content,
                ev.payload.as_ref().map(|v| v.to_string()),
                ev.tags.join(","),
                ev.source,
                ev.importance as f64,
                ev.secret as i64,
            ],
        )?;
        Ok(ev.id.clone())
    }

    /// Total count of indexed events. Cheap.
    pub fn count(&self) -> u64 {
        let conn = self.conn.lock();
        conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// List the most recent N events, newest first. Optional kind filter.
    pub fn list_recent(&self, n: usize, kind: Option<EventKind>) -> Vec<MemoryEvent> {
        let conn = self.conn.lock();
        let mut out = Vec::with_capacity(n);
        let res: rusqlite::Result<()> = (|| -> rusqlite::Result<()> {
            match kind {
                Some(k) => {
                    let mut stmt = conn.prepare(
                        "SELECT id, ts, kind, content, payload, tags, source, importance, secret
                     FROM events WHERE kind = ?1 ORDER BY ts DESC LIMIT ?2",
                    )?;
                    let rows = stmt.query_map(params![k.as_str(), n as i64], row_to_event)?;
                    for r in rows {
                        out.push(r?);
                    }
                    Ok(())
                }
                None => {
                    let mut stmt = conn.prepare(
                        "SELECT id, ts, kind, content, payload, tags, source, importance, secret
                     FROM events ORDER BY ts DESC LIMIT ?1",
                    )?;
                    let rows = stmt.query_map(params![n as i64], row_to_event)?;
                    for r in rows {
                        out.push(r?);
                    }
                    Ok(())
                }
            }
        })();
        if let Err(e) = res {
            tracing::warn!(?e, "memory: list_recent query failed");
        }
        out
    }

    /// List events in a `[from_ts, to_ts]` ms range, newest first.
    /// Used by consolidation to pick which events to archive.
    pub fn list_by_ts_range(&self, from_ts: i64, to_ts: i64) -> Vec<MemoryEvent> {
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT id, ts, kind, content, payload, tags, source, importance, secret
             FROM events WHERE ts BETWEEN ?1 AND ?2 ORDER BY ts ASC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?e, "memory: list_by_ts_range prepare failed");
                return Vec::new();
            }
        };
        let rows_iter = match stmt.query_map(params![from_ts, to_ts], row_to_event) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(?e, "memory: list_by_ts_range query failed");
                return Vec::new();
            }
        };
        rows_iter.filter_map(|r| r.ok()).collect()
    }

    /// Delete events whose `ts < cutoff_ts`. Used by `consolidate_now`
    /// after they've been written to the L3 gzip chunk.
    pub fn delete_older_than(&self, cutoff_ts: i64) -> Result<u64, super::MemoryError> {
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM events WHERE ts < ?1", params![cutoff_ts])?;
        Ok(n as u64)
    }

    /// Delete a single event by id. The JSONL line is left in place
    /// (we keep it for `rebuild_index` recovery), but the SQLite index
    /// row is removed. The dead line is collected by the next
    /// `consolidate_now` pass.
    pub fn forget_by_id(&self, id: &str) -> Result<(), super::MemoryError> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM events WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Read every line of the on-disk JSONL. Used by `rebuild_index`
    /// to repopulate the SQLite after a corruption. Skips malformed
    /// lines (logs a warning per line) rather than failing the whole
    /// rebuild.
    fn read_all_jsonl(&self) -> Vec<MemoryEvent> {
        let f = match std::fs::File::open(&self.events_path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let r = BufReader::new(f);
        let mut out = Vec::new();
        for (i, line) in r.lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(line = i, ?e, "memory: skipping undecodable line");
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<MemoryEvent>(&line) {
                Ok(ev) => out.push(ev),
                Err(e) => {
                    tracing::warn!(line = i, ?e, "memory: skipping unparseable line");
                }
            }
        }
        out
    }

    /// Wipe and re-populate the index from the JSONL. Called on
    /// startup if the index is empty, and exposed (via `consolidation`)
    /// for manual recovery.
    fn rebuild_index(events_path: &Path, index_path: &Path) -> Result<(), super::MemoryError> {
        let f = std::fs::File::open(events_path)
            .map_err(|e| super::MemoryError::Io(format!("rebuild open: {e}")))?;
        let r = BufReader::new(f);
        let mut conn = Connection::open(index_path)
            .map_err(|e| super::MemoryError::Io(format!("rebuild open index: {e}")))?;
        conn.execute_batch(
            "DROP TABLE IF EXISTS events;
             CREATE TABLE events (
                id         TEXT PRIMARY KEY,
                ts         INTEGER NOT NULL,
                kind       TEXT NOT NULL,
                content    TEXT NOT NULL,
                payload    TEXT,
                tags       TEXT NOT NULL DEFAULT '',
                source     TEXT NOT NULL DEFAULT 'agent',
                importance REAL NOT NULL DEFAULT 0.5,
                secret     INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX idx_events_ts ON events(ts DESC);
             CREATE INDEX idx_events_kind ON events(kind);
             CREATE INDEX idx_events_secret ON events(secret);
             CREATE INDEX idx_events_tags ON events(tags);
             PRAGMA journal_mode=WAL;",
        )?;
        let tx = conn.transaction()?;
        let mut count = 0u64;
        for (i, line) in r.lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }
            let ev: MemoryEvent = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(_) => {
                    tracing::warn!(line = i, "memory: rebuild skipping unparseable line");
                    continue;
                }
            };
            tx.execute(
                "INSERT OR IGNORE INTO events
                 (id, ts, kind, content, payload, tags, source, importance, secret)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    ev.id,
                    ev.ts,
                    ev.kind.as_str(),
                    ev.content,
                    ev.payload.as_ref().map(|v| v.to_string()),
                    ev.tags.join(","),
                    ev.source,
                    ev.importance as f64,
                    ev.secret as i64,
                ],
            )?;
            count += 1;
        }
        tx.commit()?;
        tracing::info!(events = count, "memory: L1 index rebuilt from JSONL");
        Ok(())
    }

    /// One-shot helper: re-read the JSONL and return everything.
    /// Currently only used by tests, but kept as part of the public
    /// surface for future "replay" tooling.
    #[allow(dead_code)]
    pub fn replay(&self) -> Vec<MemoryEvent> {
        self.read_all_jsonl()
    }
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<MemoryEvent> {
    let kind_str: String = row.get(2)?;
    let kind = EventKind::from_str(&kind_str).unwrap_or(EventKind::ToolCall);
    let payload_str: Option<String> = row.get(4)?;
    let payload = payload_str
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let tags_str: String = row.get(5)?;
    let tags: Vec<String> = if tags_str.is_empty() {
        Vec::new()
    } else {
        tags_str.split(',').map(|s| s.to_string()).collect()
    };
    let importance: f64 = row.get(7)?;
    let secret: i64 = row.get(8)?;
    Ok(MemoryEvent {
        id: row.get(0)?,
        ts: row.get(1)?,
        kind,
        content: row.get(3)?,
        payload,
        tags,
        source: row.get(6)?,
        importance: importance as f32,
        secret: secret != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::memory::schema::EventKind;
    use std::env;

    fn temp_paths() -> (PathBuf, PathBuf) {
        let mut base = env::temp_dir();
        base.push(format!("luna-mem-l1-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        (base.join("events.jsonl"), base.join("index.sqlite"))
    }

    fn ev(kind: EventKind, content: &str) -> MemoryEvent {
        MemoryEvent {
            id: uuid::Uuid::new_v4().to_string(),
            ts: crate::services::memory::now_ms(),
            kind,
            content: content.into(),
            payload: None,
            tags: vec!["test".into()],
            source: "test".into(),
            importance: 0.5,
            secret: false,
        }
    }

    #[test]
    fn append_then_list_recent() {
        let (events_p, index_p) = temp_paths();
        let l1 = L1Episodic::open(&events_p, &index_p).unwrap();
        l1.append(&ev(EventKind::ChatTurn, "first")).unwrap();
        l1.append(&ev(EventKind::FileEdit, "second")).unwrap();
        l1.append(&ev(EventKind::ChatTurn, "third")).unwrap();
        let all = l1.list_recent(10, None);
        assert_eq!(all.len(), 3);
        // Newest first.
        assert_eq!(all[0].content, "third");
        assert_eq!(all[2].content, "first");

        let only_chat = l1.list_recent(10, Some(EventKind::ChatTurn));
        assert_eq!(only_chat.len(), 2);
        for e in only_chat {
            assert_eq!(e.kind, EventKind::ChatTurn);
        }
        let _ = std::fs::remove_file(&events_p);
        let _ = std::fs::remove_file(&index_p);
    }

    #[test]
    fn reopen_rebuilds_index() {
        let (events_p, index_p) = temp_paths();
        {
            let l1 = L1Episodic::open(&events_p, &index_p).unwrap();
            l1.append(&ev(EventKind::ChatTurn, "abc")).unwrap();
            l1.append(&ev(EventKind::UserFact, "def")).unwrap();
        }
        // Wipe the SQLite index, leave the JSONL.
        std::fs::remove_file(&index_p).unwrap();
        let l1 = L1Episodic::open(&events_p, &index_p).unwrap();
        let all = l1.list_recent(10, None);
        assert_eq!(all.len(), 2, "index should have been rebuilt from JSONL");
    }

    #[test]
    fn delete_older_than() {
        let (events_p, index_p) = temp_paths();
        let l1 = L1Episodic::open(&events_p, &index_p).unwrap();
        let mut old = ev(EventKind::ChatTurn, "old");
        old.ts = 1_000; // ancient
        let mut new = ev(EventKind::ChatTurn, "new");
        new.ts = 2_000_000_000_000; // far future
        l1.append(&old).unwrap();
        l1.append(&new).unwrap();
        let n = l1.delete_older_than(1_000_000_000_000).unwrap();
        assert_eq!(n, 1);
        let left = l1.list_recent(10, None);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].content, "new");
        let _ = std::fs::remove_file(&events_p);
        let _ = std::fs::remove_file(&index_p);
    }
}
