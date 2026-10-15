//! SQLite: call index, job queue, FTS5 (F-50, F-83, F-90). `meta.json` stays the source of truth.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub struct Db {
    conn: Connection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Full pipeline: prepare → transcribe → summarize → name.
    Process,
    /// Summary only (`resummarize`, retry after `summary_pending`).
    Summarize,
}

impl JobKind {
    fn as_str(self) -> &'static str {
        match self {
            JobKind::Process => "process",
            JobKind::Summarize => "summarize",
        }
    }
    fn parse(s: &str) -> JobKind {
        if s == "summarize" {
            JobKind::Summarize
        } else {
            JobKind::Process
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: i64,
    pub call_dir: PathBuf,
    pub kind: JobKind,
    pub status: String,
    pub attempts: i64,
    pub step: Option<String>,
    pub progress: f64,
    pub error: Option<String>,
    /// JSON with per-job options (title, app, no_cloud…).
    pub options: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallRow {
    pub id: String,
    pub call_dir: String,
    pub status: String,
    pub title: Option<String>,
    pub app: String,
    pub started_at: String,
    pub duration_sec: Option<f64>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("не удалось открыть БД {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let db = Db { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Db> {
        let db = Db {
            conn: Connection::open_in_memory()?,
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS calls (
                id TEXT PRIMARY KEY,
                call_dir TEXT NOT NULL,
                status TEXT NOT NULL,
                title TEXT,
                app TEXT NOT NULL,
                started_at TEXT NOT NULL,
                duration_sec REAL,
                updated_at INTEGER NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS calls_fts USING fts5(id UNINDEXED, title, transcript, summary);
            CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                call_dir TEXT NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                next_attempt_at INTEGER NOT NULL DEFAULT 0,
                step TEXT,
                progress REAL NOT NULL DEFAULT 0,
                error TEXT,
                options TEXT NOT NULL DEFAULT '{}',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS jobs_status ON jobs(status, next_attempt_at);
            CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            "#,
        )?;
        Ok(())
    }

    // ---------- jobs ----------

    /// Enqueue; an existing unfinished job of the same kind for the same folder is reused.
    pub fn enqueue(
        &self,
        call_dir: &Path,
        kind: JobKind,
        options: &serde_json::Value,
    ) -> Result<i64> {
        let dir = call_dir.to_string_lossy().to_string();
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM jobs WHERE call_dir = ?1 AND kind = ?2 AND status IN ('queued','running','retry') LIMIT 1",
                params![dir, kind.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        let now = now_unix();
        self.conn.execute(
            "INSERT INTO jobs (call_dir, kind, status, options, created_at, updated_at) VALUES (?1, ?2, 'queued', ?3, ?4, ?4)",
            params![dir, kind.as_str(), options.to_string(), now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn row_to_job(r: &rusqlite::Row) -> rusqlite::Result<Job> {
        let opts: String = r.get(8)?;
        Ok(Job {
            id: r.get(0)?,
            call_dir: PathBuf::from(r.get::<_, String>(1)?),
            kind: JobKind::parse(&r.get::<_, String>(2)?),
            status: r.get(3)?,
            attempts: r.get(4)?,
            step: r.get(5)?,
            progress: r.get(6)?,
            error: r.get(7)?,
            options: serde_json::from_str(&opts).unwrap_or(serde_json::Value::Null),
        })
    }

    const JOB_COLS: &'static str =
        "id, call_dir, kind, status, attempts, step, progress, error, options";

    /// Next job ready to run: queued, or a retry whose backoff has passed.
    pub fn next_due(&self) -> Result<Option<Job>> {
        let sql = format!(
            "SELECT {} FROM jobs WHERE status = 'queued' OR (status = 'retry' AND next_attempt_at <= ?1) ORDER BY id LIMIT 1",
            Self::JOB_COLS
        );
        Ok(self
            .conn
            .query_row(&sql, params![now_unix()], Self::row_to_job)
            .optional()?)
    }

    pub fn pending_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status = 'queued' OR (status = 'retry' AND next_attempt_at <= ?1)",
            params![now_unix()],
            |r| r.get(0),
        )?)
    }

    pub fn job(&self, id: i64) -> Result<Option<Job>> {
        let sql = format!("SELECT {} FROM jobs WHERE id = ?1", Self::JOB_COLS);
        Ok(self
            .conn
            .query_row(&sql, params![id], Self::row_to_job)
            .optional()?)
    }

    pub fn active_jobs(&self) -> Result<Vec<Job>> {
        let sql = format!(
            "SELECT {} FROM jobs WHERE status IN ('queued','running','retry') ORDER BY id",
            Self::JOB_COLS
        );
        let mut st = self.conn.prepare(&sql)?;
        let v = st
            .query_map([], Self::row_to_job)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    pub fn recent_jobs(&self, limit: i64) -> Result<Vec<Job>> {
        let sql = format!(
            "SELECT {} FROM jobs ORDER BY id DESC LIMIT ?1",
            Self::JOB_COLS
        );
        let mut st = self.conn.prepare(&sql)?;
        let v = st
            .query_map(params![limit], Self::row_to_job)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    pub fn mark_running(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET status = 'running', attempts = attempts + 1, updated_at = ?2 WHERE id = ?1",
            params![id, now_unix()],
        )?;
        Ok(())
    }

    pub fn set_progress(&self, id: i64, step: &str, progress: f64) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET step = ?2, progress = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, step, progress, now_unix()],
        )?;
        Ok(())
    }

    pub fn set_call_dir(&self, id: i64, call_dir: &Path) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET call_dir = ?2 WHERE id = ?1",
            params![id, call_dir.to_string_lossy()],
        )?;
        Ok(())
    }

    pub fn mark_done(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET status = 'done', progress = 100, error = NULL, updated_at = ?2 WHERE id = ?1",
            params![id, now_unix()],
        )?;
        Ok(())
    }

    pub fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET status = 'failed', error = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, error, now_unix()],
        )?;
        Ok(())
    }

    /// Retry later with backoff (LLM unavailable, F-75).
    pub fn mark_retry(&self, id: i64, error: &str, after: Duration) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET status = 'retry', error = ?2, next_attempt_at = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, error, now_unix() + after.as_secs() as i64, now_unix()],
        )?;
        Ok(())
    }

    /// Make every `retry` job due now (new run, `resummarize`).
    pub fn retry_now(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET next_attempt_at = 0 WHERE status = 'retry'",
            [],
        )?;
        Ok(())
    }

    /// Jobs left `running` by a dead worker go back to the queue. Call only while holding `worker.lock`.
    pub fn recover_stale(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE jobs SET status = 'queued' WHERE status = 'running'",
            [],
        )?)
    }

    // ---------- state flags ----------

    pub fn set_flag(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO state (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn flag(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM state WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// F-51: the tray renews this heartbeat every ~30 s while OBS records. A stale heartbeat
    /// (tray crashed mid-recording) expires by itself, so the queue never waits forever.
    pub fn recording_active(&self) -> bool {
        self.flag("recording_until")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .is_some_and(|until| until > now_unix())
    }

    pub fn set_recording_active(&self, on: bool) -> Result<()> {
        let until = if on { now_unix() + 90 } else { 0 };
        self.set_flag("recording_until", &until.to_string())
    }

    // ---------- calls ----------

    pub fn upsert_call(
        &self,
        row: &CallRow,
        transcript: Option<&str>,
        summary: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO calls (id, call_dir, status, title, app, started_at, duration_sec, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET call_dir = excluded.call_dir, status = excluded.status, title = excluded.title,
               app = excluded.app, started_at = excluded.started_at, duration_sec = excluded.duration_sec, updated_at = excluded.updated_at",
            params![row.id, row.call_dir, row.status, row.title, row.app, row.started_at, row.duration_sec, now_unix()],
        )?;
        self.conn
            .execute("DELETE FROM calls_fts WHERE id = ?1", params![row.id])?;
        self.conn.execute(
            "INSERT INTO calls_fts (id, title, transcript, summary) VALUES (?1, ?2, ?3, ?4)",
            params![
                row.id,
                row.title.clone().unwrap_or_default(),
                transcript.unwrap_or(""),
                summary.unwrap_or("")
            ],
        )?;
        Ok(())
    }

    pub fn recent_calls(&self, limit: i64) -> Result<Vec<CallRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, call_dir, status, title, app, started_at, duration_sec FROM calls ORDER BY started_at DESC LIMIT ?1",
        )?;
        let v = st
            .query_map(params![limit], |r| {
                Ok(CallRow {
                    id: r.get(0)?,
                    call_dir: r.get(1)?,
                    status: r.get(2)?,
                    title: r.get(3)?,
                    app: r.get(4)?,
                    started_at: r.get(5)?,
                    duration_sec: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT id FROM calls_fts WHERE calls_fts MATCH ?1 LIMIT ?2")?;
        let v = st
            .query_map(params![query, limit], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(v)
    }

    pub fn delete_call(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM calls WHERE id = ?1", params![id])?;
        self.conn.execute("DELETE FROM calls_fts WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn clear_calls(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM calls; DELETE FROM calls_fts;")?;
        Ok(())
    }
}

/// Exclusive `worker.lock` (§4.1). Released on drop.
pub struct WorkerLock {
    _file: std::fs::File,
}

impl WorkerLock {
    pub fn try_acquire(path: &Path) -> Result<Option<WorkerLock>> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(WorkerLock { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

impl Drop for WorkerLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_lifecycle() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .enqueue(
                Path::new("C:/calls/a"),
                JobKind::Process,
                &serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(
            db.enqueue(
                Path::new("C:/calls/a"),
                JobKind::Process,
                &serde_json::json!({})
            )
            .unwrap(),
            a
        );
        let j = db.next_due().unwrap().unwrap();
        assert_eq!(j.id, a);
        db.mark_running(a).unwrap();
        assert!(db.next_due().unwrap().is_none());
        assert_eq!(db.recover_stale().unwrap(), 1);
        db.mark_retry(a, "llm down", Duration::from_secs(3600))
            .unwrap();
        assert!(db.next_due().unwrap().is_none());
        db.retry_now().unwrap();
        assert_eq!(db.next_due().unwrap().unwrap().id, a);
        db.mark_done(a).unwrap();
        assert!(db.next_due().unwrap().is_none());
    }

    #[test]
    fn fts_search() {
        let db = Db::open_in_memory().unwrap();
        let row = CallRow {
            id: "1".into(),
            call_dir: "x".into(),
            status: "done".into(),
            title: Some("Обсуждение пайплайна".into()),
            app: "Zoom".into(),
            started_at: "2026-10-09T14:30:00+03:00".into(),
            duration_sec: Some(60.0),
        };
        db.upsert_call(&row, Some("говорили про деплой"), None)
            .unwrap();
        assert_eq!(db.search("деплой", 10).unwrap(), vec!["1".to_string()]);
    }

    #[test]
    fn lock_is_exclusive() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("worker.lock");
        let l = WorkerLock::try_acquire(&p).unwrap();
        assert!(l.is_some());
        assert!(WorkerLock::try_acquire(&p).unwrap().is_none());
        drop(l);
        assert!(WorkerLock::try_acquire(&p).unwrap().is_some());
    }
}
