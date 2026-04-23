use std::{fs, path::Path};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use spotifydl_protocol::{
    AppSettings, AppSnapshot, DownloadItem, FailureReason, HistoryEntry, IntegrityFlag, ItemId,
    JobId, JobRecord, JobSource, LogEntry, OutputRecord, ProgressState, QueueStatus, ServiceHealth,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type StorageResult<T> = Result<T, StorageError>;

pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at_ms INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS app_meta (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS app_settings (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS queue_jobs (
                job_id TEXT PRIMARY KEY,
                position INTEGER NOT NULL,
                label TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_summary TEXT NOT NULL,
                source_url TEXT NOT NULL,
                state TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                started_at_ms INTEGER,
                finished_at_ms INTEGER,
                error_message TEXT,
                options_json TEXT NOT NULL,
                totals_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS queue_items (
                item_id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL REFERENCES queue_jobs(job_id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                label TEXT NOT NULL,
                url TEXT NOT NULL,
                state TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                started_at_ms INTEGER,
                finished_at_ms INTEGER,
                progress_current INTEGER NOT NULL,
                progress_total INTEGER NOT NULL,
                progress_percent INTEGER NOT NULL,
                progress_detail TEXT NOT NULL,
                failure_reason_json TEXT,
                metadata_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS history_jobs (
                history_id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL UNIQUE,
                position INTEGER NOT NULL,
                label TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_summary TEXT NOT NULL,
                source_url TEXT NOT NULL,
                final_state TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                started_at_ms INTEGER,
                finished_at_ms INTEGER,
                error_message TEXT,
                options_json TEXT NOT NULL,
                totals_json TEXT NOT NULL,
                original_inputs_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS history_items (
                history_item_id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL REFERENCES history_jobs(job_id) ON DELETE CASCADE,
                item_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                label TEXT NOT NULL,
                url TEXT NOT NULL,
                final_state TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                started_at_ms INTEGER,
                finished_at_ms INTEGER,
                progress_current INTEGER NOT NULL,
                progress_total INTEGER NOT NULL,
                progress_percent INTEGER NOT NULL,
                progress_detail TEXT NOT NULL,
                failure_reason_json TEXT,
                metadata_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS item_outputs (
                output_id INTEGER PRIMARY KEY AUTOINCREMENT,
                bucket TEXT NOT NULL,
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                disposition TEXT NOT NULL,
                final_path TEXT NOT NULL,
                artist TEXT NOT NULL,
                album TEXT NOT NULL,
                title TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                format TEXT NOT NULL,
                details_json TEXT
            );

            CREATE TABLE IF NOT EXISTS item_flags (
                flag_id INTEGER PRIMARY KEY AUTOINCREMENT,
                bucket TEXT NOT NULL,
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                severity TEXT NOT NULL,
                kind TEXT NOT NULL,
                message TEXT NOT NULL,
                details_json TEXT
            );

            CREATE TABLE IF NOT EXISTS app_logs (
                seq INTEGER PRIMARY KEY,
                timestamp_ms INTEGER NOT NULL,
                level TEXT NOT NULL,
                scope TEXT NOT NULL,
                job_id TEXT,
                item_id TEXT,
                message TEXT NOT NULL,
                details_json TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_queue_jobs_position ON queue_jobs(position);
            CREATE INDEX IF NOT EXISTS idx_queue_jobs_state ON queue_jobs(state);
            CREATE INDEX IF NOT EXISTS idx_queue_items_job_position ON queue_items(job_id, position);
            CREATE INDEX IF NOT EXISTS idx_queue_items_job_state ON queue_items(job_id, state);
            CREATE INDEX IF NOT EXISTS idx_history_jobs_position ON history_jobs(position);
            CREATE INDEX IF NOT EXISTS idx_history_jobs_finished ON history_jobs(finished_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_history_items_job_position ON history_items(job_id, position);
            CREATE INDEX IF NOT EXISTS idx_item_outputs_bucket_job_item ON item_outputs(bucket, job_id, item_id, position);
            CREATE INDEX IF NOT EXISTS idx_item_flags_bucket_job_item ON item_flags(bucket, job_id, item_id, position);
            CREATE INDEX IF NOT EXISTS idx_app_logs_timestamp ON app_logs(timestamp_ms DESC);
            "#,
        )?;

        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations (version, applied_at_ms) VALUES (?1, ?2)",
            params![1_i64, now_ms()],
        )?;

        Ok(Self { conn })
    }

    pub fn load_snapshot(&self) -> StorageResult<Option<AppSnapshot>> {
        let queue_jobs = self.load_queue_jobs()?;
        let history = self.load_history_jobs()?;
        let logs = self.load_logs()?;
        let settings = self.load_settings()?;
        let queue_status = self
            .load_meta::<QueueStatus>("queue_status")?
            .unwrap_or(QueueStatus::Idle);
        let active_job_id = self
            .load_meta::<Option<JobId>>("active_job_id")?
            .unwrap_or(None);
        let service_health = self.load_meta::<ServiceHealth>("service_health")?;

        let has_data = !queue_jobs.is_empty()
            || !history.is_empty()
            || !logs.is_empty()
            || settings.is_some()
            || service_health.is_some()
            || self.load_meta::<QueueStatus>("queue_status")?.is_some()
            || self.load_meta::<Option<JobId>>("active_job_id")?.is_some();

        if !has_data {
            return Ok(None);
        }

        let mut snapshot = AppSnapshot::default();
        snapshot.queue.jobs = queue_jobs;
        snapshot.queue.status = queue_status;
        snapshot.queue.active_job_id = active_job_id;
        snapshot.history = history;
        snapshot.logs = logs;
        snapshot.settings = settings.unwrap_or_default();
        snapshot.service_health = service_health.unwrap_or_default();

        Ok(Some(snapshot))
    }

    pub fn save_snapshot(&mut self, snapshot: &AppSnapshot) -> StorageResult<()> {
        let tx = self.conn.transaction()?;

        tx.execute("DELETE FROM queue_items", [])?;
        tx.execute("DELETE FROM queue_jobs", [])?;
        tx.execute("DELETE FROM history_items", [])?;
        tx.execute("DELETE FROM history_jobs", [])?;
        tx.execute("DELETE FROM item_outputs", [])?;
        tx.execute("DELETE FROM item_flags", [])?;
        tx.execute("DELETE FROM app_logs", [])?;
        tx.execute("DELETE FROM app_settings", [])?;
        tx.execute("DELETE FROM app_meta", [])?;

        Self::save_settings(&tx, &snapshot.settings)?;
        Self::save_meta(&tx, "queue_status", &snapshot.queue.status)?;
        Self::save_meta(&tx, "active_job_id", &snapshot.queue.active_job_id)?;
        Self::save_meta(&tx, "service_health", &snapshot.service_health)?;

        for job in &snapshot.queue.jobs {
            Self::insert_queue_job(&tx, job)?;
        }

        for (position, entry) in snapshot.history.iter().enumerate() {
            Self::insert_history_job(&tx, entry, position as i64)?;
        }

        for log in &snapshot.logs {
            tx.execute(
                r#"
                INSERT INTO app_logs (
                    seq, timestamp_ms, level, scope, job_id, item_id, message, details_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    log.seq as i64,
                    log.timestamp_ms,
                    to_json(&log.level)?,
                    to_json(&log.scope)?,
                    log.job_id.as_ref().map(|id| id.0.clone()),
                    log.item_id.as_ref().map(|id| id.0.clone()),
                    log.message,
                    opt_json(&log.details)?,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    fn insert_queue_job(tx: &Transaction<'_>, job: &JobRecord) -> StorageResult<()> {
        tx.execute(
            r#"
            INSERT INTO queue_jobs (
                job_id, position, label, source_kind, source_summary, source_url, state,
                created_at_ms, updated_at_ms, started_at_ms, finished_at_ms, error_message,
                options_json, totals_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
            params![
                job.id.0,
                job.position as i64,
                job.label,
                to_json(&job.source.kind)?,
                job.source.summary,
                job.source_url,
                to_json(&job.state)?,
                job.created_at_ms,
                job.updated_at_ms,
                job.started_at_ms,
                job.finished_at_ms,
                job.error_message,
                to_json(&job.options)?,
                to_json(&job.totals)?,
            ],
        )?;

        for (position, item) in job.items.iter().enumerate() {
            Self::insert_item(tx, "queue", &job.id, item, position as i64, false)?;
        }

        Ok(())
    }

    fn insert_history_job(
        tx: &Transaction<'_>,
        entry: &HistoryEntry,
        position: i64,
    ) -> StorageResult<()> {
        let job = &entry.job;
        tx.execute(
            r#"
            INSERT INTO history_jobs (
                job_id, position, label, source_kind, source_summary, source_url, final_state,
                created_at_ms, updated_at_ms, started_at_ms, finished_at_ms, error_message,
                options_json, totals_json, original_inputs_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            "#,
            params![
                job.id.0,
                position,
                job.label,
                to_json(&job.source.kind)?,
                job.source.summary,
                job.source_url,
                to_json(&job.state)?,
                job.created_at_ms,
                job.updated_at_ms,
                job.started_at_ms,
                job.finished_at_ms,
                job.error_message,
                to_json(&job.options)?,
                to_json(&job.totals)?,
                to_json(&entry.original_inputs)?,
            ],
        )?;

        for (item_position, item) in job.items.iter().enumerate() {
            Self::insert_item(tx, "history", &job.id, item, item_position as i64, true)?;
        }

        Ok(())
    }

    fn insert_item(
        tx: &Transaction<'_>,
        bucket: &str,
        job_id: &JobId,
        item: &DownloadItem,
        position: i64,
        history: bool,
    ) -> StorageResult<()> {
        let table = if history {
            "history_items"
        } else {
            "queue_items"
        };
        let state_column = if history { "final_state" } else { "state" };
        let sql = format!(
            r#"
            INSERT INTO {table} (
                item_id, job_id, position, label, url, {state_column},
                created_at_ms, updated_at_ms, started_at_ms, finished_at_ms,
                progress_current, progress_total, progress_percent, progress_detail,
                failure_reason_json, metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            "#
        );
        tx.execute(
            &sql,
            params![
                item.id.0,
                job_id.0,
                position,
                item.label,
                item.url,
                to_json(&item.state)?,
                item.created_at_ms,
                item.updated_at_ms,
                item.started_at_ms,
                item.finished_at_ms,
                item.progress.current as i64,
                item.progress.total as i64,
                item.progress.percent as i64,
                item.progress.detail,
                opt_json(&item.failure_reason)?,
                to_json(&item.metadata)?,
            ],
        )?;

        for (output_position, output) in item.outputs.iter().enumerate() {
            tx.execute(
                r#"
                INSERT INTO item_outputs (
                    bucket, job_id, item_id, position, disposition, final_path, artist, album,
                    title, size_bytes, format, details_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    bucket,
                    job_id.0,
                    item.id.0,
                    output_position as i64,
                    to_json(&output.disposition)?,
                    output.final_path,
                    output.artist,
                    output.album,
                    output.title,
                    output.size_bytes as i64,
                    output.format,
                    opt_json(&output.details)?,
                ],
            )?;
        }

        for (flag_position, flag) in item.flags.iter().enumerate() {
            tx.execute(
                r#"
                INSERT INTO item_flags (
                    bucket, job_id, item_id, position, severity, kind, message, details_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    bucket,
                    job_id.0,
                    item.id.0,
                    flag_position as i64,
                    to_json(&flag.severity)?,
                    flag.kind,
                    flag.message,
                    opt_json(&flag.details)?,
                ],
            )?;
        }

        Ok(())
    }

    fn load_queue_jobs(&self) -> StorageResult<Vec<JobRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                job_id, position, label, source_kind, source_summary, source_url, state,
                created_at_ms, updated_at_ms, started_at_ms, finished_at_ms, error_message,
                options_json, totals_json
            FROM queue_jobs
            ORDER BY position ASC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, String>(13)?,
            ))
        })?;

        let mut jobs = Vec::new();
        for row in rows {
            let (
                job_id,
                position,
                label,
                source_kind,
                source_summary,
                source_url,
                state,
                created_at_ms,
                updated_at_ms,
                started_at_ms,
                finished_at_ms,
                error_message,
                options_json,
                totals_json,
            ) = row?;

            let job_id = JobId(job_id);
            jobs.push(JobRecord {
                id: job_id.clone(),
                source_url,
                source: JobSource {
                    kind: from_json(&source_kind)?,
                    summary: source_summary,
                },
                label,
                state: from_json(&state)?,
                progress: self.load_progress("queue_items", &job_id)?,
                items: self.load_items("queue", "queue_items", &job_id)?,
                position: position as u32,
                created_at_ms,
                updated_at_ms,
                started_at_ms,
                finished_at_ms,
                error_message,
                options: from_json(&options_json)?,
                totals: from_json(&totals_json)?,
            });
        }

        Ok(jobs)
    }

    fn load_history_jobs(&self) -> StorageResult<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                job_id, position, label, source_kind, source_summary, source_url, final_state,
                created_at_ms, updated_at_ms, started_at_ms, finished_at_ms, error_message,
                options_json, totals_json, original_inputs_json
            FROM history_jobs
            ORDER BY position ASC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, String>(14)?,
            ))
        })?;

        let mut history = Vec::new();
        for row in rows {
            let (
                job_id,
                position,
                label,
                source_kind,
                source_summary,
                source_url,
                final_state,
                created_at_ms,
                updated_at_ms,
                started_at_ms,
                finished_at_ms,
                error_message,
                options_json,
                totals_json,
                original_inputs_json,
            ) = row?;

            let job_id = JobId(job_id);
            history.push(HistoryEntry {
                original_inputs: from_json(&original_inputs_json)?,
                job: JobRecord {
                    id: job_id.clone(),
                    source_url,
                    source: JobSource {
                        kind: from_json(&source_kind)?,
                        summary: source_summary,
                    },
                    label,
                    state: from_json(&final_state)?,
                    progress: self.load_progress("history_items", &job_id)?,
                    items: self.load_items("history", "history_items", &job_id)?,
                    position: position as u32,
                    created_at_ms,
                    updated_at_ms,
                    started_at_ms,
                    finished_at_ms,
                    error_message,
                    options: from_json(&options_json)?,
                    totals: from_json(&totals_json)?,
                },
            });
        }

        Ok(history)
    }

    fn load_progress(&self, table: &str, job_id: &JobId) -> StorageResult<ProgressState> {
        let sql = format!(
            "SELECT progress_current, progress_total, progress_percent, progress_detail FROM {table} WHERE job_id = ?1 ORDER BY position DESC LIMIT 1"
        );
        let row = self
            .conn
            .query_row(&sql, [job_id.0.as_str()], |row| {
                Ok(ProgressState {
                    current: row.get::<_, i64>(0)? as u32,
                    total: row.get::<_, i64>(1)? as u32,
                    percent: row.get::<_, i64>(2)? as u8,
                    detail: row.get::<_, String>(3)?,
                })
            })
            .optional()?;

        Ok(row.unwrap_or_default())
    }

    fn load_items(
        &self,
        bucket: &str,
        table: &str,
        job_id: &JobId,
    ) -> StorageResult<Vec<DownloadItem>> {
        let state_column = if table == "history_items" {
            "final_state"
        } else {
            "state"
        };
        let sql = format!(
            r#"
            SELECT
                item_id, label, url, {state_column}, created_at_ms, updated_at_ms,
                started_at_ms, finished_at_ms, progress_current, progress_total,
                progress_percent, progress_detail, failure_reason_json, metadata_json
            FROM {table}
            WHERE job_id = ?1
            ORDER BY position ASC
            "#
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([job_id.0.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, String>(13)?,
            ))
        })?;

        let mut items = Vec::new();
        for row in rows {
            let (
                item_id,
                label,
                url,
                state,
                created_at_ms,
                updated_at_ms,
                started_at_ms,
                finished_at_ms,
                progress_current,
                progress_total,
                progress_percent,
                progress_detail,
                failure_reason_json,
                metadata_json,
            ) = row?;

            let item_id = ItemId(item_id);
            items.push(DownloadItem {
                id: item_id.clone(),
                label,
                url,
                state: from_json(&state)?,
                progress: ProgressState {
                    current: progress_current as u32,
                    total: progress_total as u32,
                    percent: progress_percent as u8,
                    detail: progress_detail,
                },
                created_at_ms,
                updated_at_ms,
                started_at_ms,
                finished_at_ms,
                failure_reason: match failure_reason_json {
                    Some(value) => from_json(&value)?,
                    None => None::<FailureReason>,
                },
                metadata: from_json(&metadata_json)?,
                outputs: self.load_outputs(bucket, job_id, &item_id)?,
                flags: self.load_flags(bucket, job_id, &item_id)?,
            });
        }

        Ok(items)
    }

    fn load_outputs(
        &self,
        bucket: &str,
        job_id: &JobId,
        item_id: &ItemId,
    ) -> StorageResult<Vec<OutputRecord>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT disposition, final_path, artist, album, title, size_bytes, format, details_json
            FROM item_outputs
            WHERE bucket = ?1 AND job_id = ?2 AND item_id = ?3
            ORDER BY position ASC
            "#,
        )?;
        let rows = stmt.query_map(
            params![bucket, job_id.0.as_str(), item_id.0.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )?;

        let mut outputs = Vec::new();
        for row in rows {
            let (disposition, final_path, artist, album, title, size_bytes, format, details_json) =
                row?;
            outputs.push(OutputRecord {
                disposition: from_json(&disposition)?,
                final_path,
                artist,
                album,
                title,
                size_bytes: size_bytes as u64,
                format,
                details: match details_json {
                    Some(value) => from_json(&value)?,
                    None => None::<String>,
                },
            });
        }

        Ok(outputs)
    }

    fn load_flags(
        &self,
        bucket: &str,
        job_id: &JobId,
        item_id: &ItemId,
    ) -> StorageResult<Vec<IntegrityFlag>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT severity, kind, message, details_json
            FROM item_flags
            WHERE bucket = ?1 AND job_id = ?2 AND item_id = ?3
            ORDER BY position ASC
            "#,
        )?;
        let rows = stmt.query_map(
            params![bucket, job_id.0.as_str(), item_id.0.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )?;

        let mut flags = Vec::new();
        for row in rows {
            let (severity, kind, message, details_json) = row?;
            flags.push(IntegrityFlag {
                severity: from_json(&severity)?,
                kind,
                message,
                details: match details_json {
                    Some(value) => from_json(&value)?,
                    None => None::<String>,
                },
            });
        }

        Ok(flags)
    }

    fn load_logs(&self) -> StorageResult<Vec<LogEntry>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT seq, timestamp_ms, level, scope, job_id, item_id, message, details_json
            FROM app_logs
            ORDER BY seq ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;

        let mut logs = Vec::new();
        for row in rows {
            let (seq, timestamp_ms, level, scope, job_id, item_id, message, details_json) = row?;
            logs.push(LogEntry {
                seq: seq as u64,
                timestamp_ms,
                level: from_json(&level)?,
                scope: from_json(&scope)?,
                job_id: job_id.map(JobId),
                item_id: item_id.map(ItemId),
                message,
                details: match details_json {
                    Some(value) => from_json(&value)?,
                    None => None::<String>,
                },
            });
        }

        Ok(logs)
    }

    fn load_settings(&self) -> StorageResult<Option<AppSettings>> {
        let default_destination = self.load_setting::<String>("default_destination")?;
        let default_format = self.load_setting::<String>("default_format")?;
        let max_parallel = self.load_setting::<u16>("max_parallel")?;
        let max_history_entries = self.load_setting::<usize>("max_history_entries")?;
        let max_log_entries = self.load_setting::<usize>("max_log_entries")?;
        let theme_mode = self.load_setting("theme_mode")?;
        let database_path = self.load_setting::<String>("database_path")?;
        let preferred_backend = self.load_setting("preferred_backend")?;
        let external_backend_executable =
            self.load_setting::<String>("external_backend_executable")?;

        let has_any = default_destination.is_some()
            || default_format.is_some()
            || max_parallel.is_some()
            || max_history_entries.is_some()
            || max_log_entries.is_some()
            || theme_mode.is_some()
            || database_path.is_some()
            || preferred_backend.is_some()
            || external_backend_executable.is_some();

        if !has_any {
            return Ok(None);
        }

        let mut settings = AppSettings::default();
        if let Some(value) = default_destination {
            settings.default_destination = value;
        }
        if let Some(value) = default_format {
            settings.default_format = value;
        }
        if let Some(value) = max_parallel {
            settings.max_parallel = value;
        }
        if let Some(value) = max_history_entries {
            settings.max_history_entries = value;
        }
        if let Some(value) = max_log_entries {
            settings.max_log_entries = value;
        }
        if let Some(value) = theme_mode {
            settings.theme_mode = value;
        }
        if let Some(value) = database_path {
            settings.database_path = value;
        }
        if let Some(value) = preferred_backend {
            settings.preferred_backend = value;
        }
        if let Some(value) = external_backend_executable {
            settings.external_backend_executable = value;
        }

        Ok(Some(settings))
    }

    fn save_settings(tx: &Transaction<'_>, settings: &AppSettings) -> StorageResult<()> {
        let timestamp = now_ms();
        Self::save_setting(tx, "database_path", &settings.database_path, timestamp)?;
        Self::save_setting(
            tx,
            "default_destination",
            &settings.default_destination,
            timestamp,
        )?;
        Self::save_setting(tx, "default_format", &settings.default_format, timestamp)?;
        Self::save_setting(tx, "max_parallel", &settings.max_parallel, timestamp)?;
        Self::save_setting(
            tx,
            "max_history_entries",
            &settings.max_history_entries,
            timestamp,
        )?;
        Self::save_setting(tx, "max_log_entries", &settings.max_log_entries, timestamp)?;
        Self::save_setting(tx, "theme_mode", &settings.theme_mode, timestamp)?;
        Self::save_setting(
            tx,
            "preferred_backend",
            &settings.preferred_backend,
            timestamp,
        )?;
        Self::save_setting(
            tx,
            "external_backend_executable",
            &settings.external_backend_executable,
            timestamp,
        )?;
        Ok(())
    }

    fn save_setting<T: Serialize>(
        tx: &Transaction<'_>,
        key: &str,
        value: &T,
        updated_at_ms: i64,
    ) -> StorageResult<()> {
        tx.execute(
            "INSERT INTO app_settings (key, value_json, updated_at_ms) VALUES (?1, ?2, ?3)",
            params![key, to_json(value)?, updated_at_ms],
        )?;
        Ok(())
    }

    fn load_setting<T: DeserializeOwned>(&self, key: &str) -> StorageResult<Option<T>> {
        let value = self
            .conn
            .query_row(
                "SELECT value_json FROM app_settings WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        match value {
            Some(raw) => Ok(Some(from_json(&raw)?)),
            None => Ok(None),
        }
    }

    fn save_meta<T: Serialize>(tx: &Transaction<'_>, key: &str, value: &T) -> StorageResult<()> {
        tx.execute(
            "INSERT INTO app_meta (key, value_json) VALUES (?1, ?2)",
            params![key, to_json(value)?],
        )?;
        Ok(())
    }

    fn load_meta<T>(&self, key: &str) -> StorageResult<Option<T>>
    where
        T: DeserializeOwned,
    {
        let value = self
            .conn
            .query_row(
                "SELECT value_json FROM app_meta WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        match value {
            Some(raw) => Ok(Some(from_json(&raw)?)),
            None => Ok(None),
        }
    }
}

fn to_json<T: Serialize>(value: &T) -> StorageResult<String> {
    Ok(serde_json::to_string(value)?)
}

fn opt_json<T: Serialize>(value: &Option<T>) -> StorageResult<Option<String>> {
    match value {
        Some(value) => Ok(Some(serde_json::to_string(value)?)),
        None => Ok(None),
    }
}

fn from_json<T: DeserializeOwned>(raw: &str) -> StorageResult<T> {
    Ok(serde_json::from_str(raw)?)
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
