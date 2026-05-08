use crate::sync::errors::{Code, SyncError};
use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS resources (
    provider       TEXT    NOT NULL,
    kind           TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    file_path      TEXT    NOT NULL,
    remote_id      TEXT,
    desired_hash   TEXT    NOT NULL,
    applied_hash   TEXT,
    last_applied   INTEGER,
    PRIMARY KEY (provider, kind, name)
);

CREATE TABLE IF NOT EXISTS apply_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    provider      TEXT    NOT NULL,
    kind          TEXT    NOT NULL,
    name          TEXT    NOT NULL,
    action        TEXT    NOT NULL,
    started_at    INTEGER NOT NULL,
    finished_at   INTEGER,
    outcome       TEXT,
    error         TEXT,
    remote_id     TEXT
);
"#;

pub struct State {
    pub conn: Connection,
}

#[derive(Debug, Clone)]
pub struct ResourceRow {
    pub provider: String,
    pub kind: String,
    pub name: String,
    pub file_path: String,
    pub remote_id: Option<String>,
    pub desired_hash: String,
    pub applied_hash: Option<String>,
    pub last_applied: Option<i64>,
}

impl State {
    pub fn open(db_path: &Path) -> Result<Self, SyncError> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                SyncError::new(
                    Code::StateDb,
                    format!("Failed to create {}: {}", parent.display(), e),
                    "Check filesystem permissions on the workspace root.",
                )
            })?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            SyncError::new(
                Code::StateDb,
                format!("Failed to open {}: {}", db_path.display(), e),
                "Delete the file and re-run `bisque-sync import` to rebuild state.",
            )
        })?;
        conn.execute_batch(SCHEMA).map_err(|e| {
            SyncError::new(
                Code::StateDb,
                format!("Failed to initialize schema: {e}"),
                "Delete `.bisque/state.db` and try again.",
            )
        })?;
        Ok(State { conn })
    }

    pub fn count_resources(&self) -> Result<i64, SyncError> {
        self.conn
            .query_row("SELECT COUNT(*) FROM resources", [], |r| r.get::<_, i64>(0))
            .map_err(|e| SyncError::new(Code::StateDb, format!("count_resources: {e}"), ""))
    }

    pub fn get_resource(
        &self,
        provider: &str,
        kind: &str,
        name: &str,
    ) -> Result<Option<ResourceRow>, SyncError> {
        self.conn
            .query_row(
                "SELECT provider, kind, name, file_path, remote_id, desired_hash, applied_hash, last_applied
                 FROM resources WHERE provider = ?1 AND kind = ?2 AND name = ?3",
                params![provider, kind, name],
                |row| {
                    Ok(ResourceRow {
                        provider: row.get(0)?,
                        kind: row.get(1)?,
                        name: row.get(2)?,
                        file_path: row.get(3)?,
                        remote_id: row.get(4)?,
                        desired_hash: row.get(5)?,
                        applied_hash: row.get(6)?,
                        last_applied: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(|e| SyncError::new(Code::StateDb, format!("get_resource: {e}"), ""))
    }

    pub fn list_resources(
        &self,
        provider_filter: Option<&str>,
        kind_filter: Option<&str>,
    ) -> Result<Vec<ResourceRow>, SyncError> {
        let mut sql = String::from(
            "SELECT provider, kind, name, file_path, remote_id, desired_hash, applied_hash, last_applied
             FROM resources",
        );
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<String> = Vec::new();
        if let Some(p) = provider_filter {
            clauses.push(format!("provider = ?{}", clauses.len() + 1));
            args.push(p.to_string());
        }
        if let Some(k) = kind_filter {
            clauses.push(format!("kind = ?{}", clauses.len() + 1));
            args.push(k.to_string());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY provider, kind, name");

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| SyncError::new(Code::StateDb, format!("list_resources prepare: {e}"), ""))?;

        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |row| {
                Ok(ResourceRow {
                    provider: row.get(0)?,
                    kind: row.get(1)?,
                    name: row.get(2)?,
                    file_path: row.get(3)?,
                    remote_id: row.get(4)?,
                    desired_hash: row.get(5)?,
                    applied_hash: row.get(6)?,
                    last_applied: row.get(7)?,
                })
            })
            .map_err(|e| SyncError::new(Code::StateDb, format!("list_resources query: {e}"), ""))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| {
                SyncError::new(Code::StateDb, format!("list_resources row: {e}"), "")
            })?);
        }
        Ok(out)
    }

    pub fn upsert_resource(&self, row: &ResourceRow) -> Result<(), SyncError> {
        self.conn
            .execute(
                "INSERT INTO resources
                    (provider, kind, name, file_path, remote_id, desired_hash, applied_hash, last_applied)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(provider, kind, name) DO UPDATE SET
                    file_path = excluded.file_path,
                    remote_id = COALESCE(excluded.remote_id, resources.remote_id),
                    desired_hash = excluded.desired_hash,
                    applied_hash = COALESCE(excluded.applied_hash, resources.applied_hash),
                    last_applied = COALESCE(excluded.last_applied, resources.last_applied)",
                params![
                    row.provider,
                    row.kind,
                    row.name,
                    row.file_path,
                    row.remote_id,
                    row.desired_hash,
                    row.applied_hash,
                    row.last_applied,
                ],
            )
            .map_err(|e| SyncError::new(Code::StateDb, format!("upsert_resource: {e}"), ""))?;
        Ok(())
    }

    pub fn mark_applied(
        &self,
        provider: &str,
        kind: &str,
        name: &str,
        remote_id: Option<&str>,
        applied_hash: &str,
    ) -> Result<(), SyncError> {
        let now = now_unix();
        self.conn
            .execute(
                "UPDATE resources
                 SET remote_id = COALESCE(?4, remote_id),
                     applied_hash = ?5,
                     last_applied = ?6
                 WHERE provider = ?1 AND kind = ?2 AND name = ?3",
                params![provider, kind, name, remote_id, applied_hash, now],
            )
            .map_err(|e| SyncError::new(Code::StateDb, format!("mark_applied: {e}"), ""))?;
        Ok(())
    }

    pub fn log_apply_start(
        &self,
        provider: &str,
        kind: &str,
        name: &str,
        action: &str,
    ) -> Result<i64, SyncError> {
        let now = now_unix();
        self.conn
            .execute(
                "INSERT INTO apply_log (provider, kind, name, action, started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![provider, kind, name, action, now],
            )
            .map_err(|e| SyncError::new(Code::StateDb, format!("log_apply_start: {e}"), ""))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn log_apply_finish(
        &self,
        id: i64,
        outcome: &str,
        error: Option<&str>,
        remote_id: Option<&str>,
    ) -> Result<(), SyncError> {
        let now = now_unix();
        self.conn
            .execute(
                "UPDATE apply_log SET finished_at = ?1, outcome = ?2, error = ?3, remote_id = ?4 WHERE id = ?5",
                params![now, outcome, error, remote_id, id],
            )
            .map_err(|e| SyncError::new(Code::StateDb, format!("log_apply_finish: {e}"), ""))?;
        Ok(())
    }
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
