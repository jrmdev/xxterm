use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Group {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Session {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: String,
    pub agent_forwarding: bool,
    pub private_key_path: Option<String>,
    pub jump_host_session_id: Option<i64>,
    pub startup_command: Option<String>,
    pub group_id: Option<i64>,
    pub group_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSessionInput {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: String,
    pub agent_forwarding: bool,
    pub private_key_path: Option<String>,
    pub jump_host_session_id: Option<i64>,
    pub startup_command: Option<String>,
    pub group_id: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateSessionInput {
    pub name: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub auth_method: Option<String>,
    pub agent_forwarding: Option<bool>,
    pub private_key_path: Option<String>,
    pub jump_host_session_id: Option<i64>,
    pub startup_command: Option<String>,
    pub group_id: Option<i64>,
}

pub struct SessionStore {
    conn: Mutex<Connection>,
}

impl SessionStore {
    pub fn new() -> Result<Self, String> {
        let app_dir: PathBuf = dirs::home_dir()
            .ok_or_else(|| "Failed to resolve home directory".to_string())?
            .join(".config")
            .join("xxterm");
        std::fs::create_dir_all(&app_dir)
            .map_err(|e| format!("Failed to create app dir: {}", e))?;
        let db_path = app_dir.join("xxterm.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("Failed to open database: {}", e))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS groups (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                host TEXT NOT NULL,
                port INTEGER NOT NULL DEFAULT 22,
                username TEXT NOT NULL,
                auth_method TEXT NOT NULL CHECK(auth_method IN ('password', 'key', 'agent')),
                agent_forwarding INTEGER NOT NULL DEFAULT 0,
                private_key_path TEXT,
                jump_host_session_id INTEGER,
                startup_command TEXT,
                group_id INTEGER,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS port_forwards (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                forward_type TEXT NOT NULL CHECK(forward_type IN ('local', 'remote', 'dynamic')),
                local_port INTEGER NOT NULL,
                local_host TEXT NOT NULL DEFAULT '',
                remote_host TEXT,
                remote_port INTEGER,
                always_start INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_port_forwards_session ON port_forwards(session_id);",
        )
        .map_err(|e| format!("Failed to create schema: {}", e))?;

        // Migration: add local_host column if missing (databases created before this column existed)
        let _ = conn.execute_batch(
            "ALTER TABLE port_forwards ADD COLUMN local_host TEXT NOT NULL DEFAULT '';",
        );
        // Migration: add jump_host_session_id column if missing
        let _ = conn.execute_batch("ALTER TABLE sessions ADD COLUMN jump_host_session_id INTEGER;");
        // Migration: add group_id column if missing
        let _ = conn.execute_batch("ALTER TABLE sessions ADD COLUMN group_id INTEGER;");
        // Migration: add agent_forwarding column if missing
        let _ = conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN agent_forwarding INTEGER NOT NULL DEFAULT 0;",
        );
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get::<_, i32>(3)? as u16,
        username: row.get(4)?,
        auth_method: row.get(5)?,
        agent_forwarding: row.get::<_, i32>(6)? != 0,
        private_key_path: row.get(7)?,
        jump_host_session_id: row.get(8)?,
        startup_command: row.get(9)?,
        group_id: row.get(10)?,
        group_name: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

const SESSION_COLUMNS: &str = "s.id, s.name, s.host, s.port, s.username, s.auth_method, s.agent_forwarding, s.private_key_path, \
     s.jump_host_session_id, s.startup_command, s.group_id, g.name AS group_name, \
     s.created_at, s.updated_at";

impl SessionStore {
    pub fn list_sessions(&self) -> Result<Vec<Session>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM sessions s \
                 LEFT JOIN groups g ON s.group_id = g.id ORDER BY s.name"
            ))
            .map_err(|e| e.to_string())?;
        let sessions = stmt
            .query_map([], session_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(sessions)
    }

    pub fn get_session(&self, id: i64) -> Result<Session, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            &format!(
                "SELECT {SESSION_COLUMNS} FROM sessions s \
                 LEFT JOIN groups g ON s.group_id = g.id WHERE s.id = ?1"
            ),
            params![id],
            session_from_row,
        )
        .map_err(|e| e.to_string())
    }

    pub fn create_session(&self, input: CreateSessionInput) -> Result<Session, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO sessions (name, host, port, username, auth_method, agent_forwarding, private_key_path, jump_host_session_id, startup_command, group_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                input.name,
                input.host,
                input.port as i32,
                input.username,
                input.auth_method,
                if input.agent_forwarding { 1 } else { 0 },
                input.private_key_path,
                input.jump_host_session_id,
                input.startup_command,
                input.group_id,
            ],
        )
        .map_err(|e| format!("Failed to create session: {}", e))?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.get_session(id)
    }

    pub fn update_session(&self, id: i64, input: UpdateSessionInput) -> Result<Session, String> {
        let existing = self.get_session(id)?;
        let old_group_id = existing.group_id;
        let new_group_id = input.group_id;
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE sessions SET name=?1, host=?2, port=?3, username=?4, auth_method=?5, \
             agent_forwarding=?6, private_key_path=?7, jump_host_session_id=?8, startup_command=?9, group_id=?10, updated_at=datetime('now') WHERE id=?11",
            params![
                input.name.unwrap_or(existing.name),
                input.host.unwrap_or(existing.host),
                input.port.unwrap_or(existing.port) as i32,
                input.username.unwrap_or(existing.username),
                input.auth_method.unwrap_or(existing.auth_method),
                if input.agent_forwarding.unwrap_or(existing.agent_forwarding) { 1 } else { 0 },
                input.private_key_path.or(existing.private_key_path),
                input.jump_host_session_id,
                input.startup_command.or(existing.startup_command),
                input.group_id,
                id,
            ],
        )
        .map_err(|e| format!("Failed to update session: {}", e))?;
        drop(conn);
        if let Some(old_gid) = old_group_id {
            if Some(old_gid) != new_group_id {
                self.cleanup_empty_group(old_gid)?;
            }
        }
        self.get_session(id)
    }

    pub fn delete_session(&self, id: i64) -> Result<(), String> {
        let session = self.get_session(id)?;
        let group_id = session.group_id;
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE sessions SET jump_host_session_id = NULL WHERE jump_host_session_id = ?1",
            params![id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        drop(conn);
        if let Some(gid) = group_id {
            self.cleanup_empty_group(gid)?;
        }
        Ok(())
    }

    fn cleanup_empty_group(&self, group_id: i64) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE group_id = ?1",
                params![group_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if count == 0 {
            conn.execute("DELETE FROM groups WHERE id = ?1", params![group_id])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn list_groups(&self) -> Result<Vec<Group>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, name FROM groups ORDER BY name")
            .map_err(|e| e.to_string())?;
        let groups = stmt
            .query_map([], |row| {
                Ok(Group {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(groups)
    }

    pub fn create_group(&self, name: String) -> Result<Group, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("INSERT INTO groups (name) VALUES (?1)", params![&name])
            .map_err(|e| format!("Failed to create group: {}", e))?;
        let id = conn.last_insert_rowid();
        Ok(Group { id, name })
    }

    pub fn delete_group(&self, id: i64) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE sessions SET group_id = NULL WHERE group_id = ?1",
            params![id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM groups WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT value FROM settings WHERE key = ?1")
            .map_err(|e| e.to_string())?;
        let result: Result<Option<String>, _> = stmt
            .query_row(params![key], |row| row.get(0))
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            });
        result.map_err(|e| e.to_string())
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|e| format!("Failed to set setting: {}", e))?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_encrypted_secret_ids(&self) -> Result<Vec<(String, i64)>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let prefixes = ["encrypted_pwd_", "encrypted_key_passphrase_"];
        let mut stmt = conn
            .prepare("SELECT key FROM settings WHERE key LIKE 'encrypted_pwd_%' OR key LIKE 'encrypted_key_passphrase_%'")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                let key: String = row.get(0)?;
                Ok(key)
            })
            .map_err(|e| e.to_string())?;
        let mut ids = Vec::new();
        for row in rows {
            let key = row.map_err(|e| e.to_string())?;
            for prefix in prefixes {
                if let Some(id_str) = key.strip_prefix(prefix) {
                    if let Ok(id) = id_str.parse::<i64>() {
                        ids.push((prefix.to_string(), id));
                    }
                    break;
                }
            }
        }
        Ok(ids)
    }

    pub fn import_from_ssh_config(&self, entries: Vec<SshConfigEntry>) -> Result<usize, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut imported = 0;
        for entry in &entries {
            let result = conn.execute(
                "INSERT OR IGNORE INTO sessions (name, host, port, username, auth_method, agent_forwarding, private_key_path, jump_host_session_id) \
                 VALUES (?1, ?2, ?3, ?4, 'key', 0, ?5, NULL)",
                params![
                    entry.host,
                    entry.host_name.as_deref().unwrap_or(&entry.host),
                    entry.port.map(|p| p as i32).unwrap_or(22),
                    entry.user.as_deref().unwrap_or("root"),
                    entry.identity_file.as_deref(),
                ],
            );
            if let Ok(rows) = result {
                if rows > 0 {
                    imported += 1;
                }
            }
        }
        Ok(imported)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PortForward {
    pub id: i64,
    pub session_id: i64,
    pub forward_type: String,
    pub local_port: u16,
    pub local_host: String,
    pub remote_host: Option<String>,
    pub remote_port: Option<u16>,
    pub always_start: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreatePortForwardInput {
    pub session_id: i64,
    pub forward_type: String,
    pub local_port: u16,
    pub local_host: String,
    pub remote_host: Option<String>,
    pub remote_port: Option<u16>,
    pub always_start: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdatePortForwardInput {
    pub forward_type: Option<String>,
    pub local_port: Option<u16>,
    pub local_host: Option<String>,
    pub remote_host: Option<Option<String>>,
    pub remote_port: Option<Option<u16>>,
    pub always_start: Option<bool>,
}

fn port_forward_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PortForward> {
    Ok(PortForward {
        id: row.get(0)?,
        session_id: row.get(1)?,
        forward_type: row.get(2)?,
        local_port: row.get::<_, i32>(3)? as u16,
        local_host: row.get::<_, String>(4)?,
        remote_host: row.get(5)?,
        remote_port: row.get::<_, Option<i32>>(6)?.map(|v| v as u16),
        always_start: row.get::<_, i32>(7)? != 0,
    })
}

const PORT_FORWARD_COLUMNS: &str =
    "id, session_id, forward_type, local_port, local_host, remote_host, remote_port, always_start";

impl SessionStore {
    pub fn list_port_forwards(&self, session_id: i64) -> Result<Vec<PortForward>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {PORT_FORWARD_COLUMNS} FROM port_forwards WHERE session_id = ?1 ORDER BY id"
            ))
            .map_err(|e| e.to_string())?;
        let forwards = stmt
            .query_map(params![session_id], port_forward_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(forwards)
    }

    pub fn create_port_forward(
        &self,
        input: CreatePortForwardInput,
    ) -> Result<PortForward, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO port_forwards (session_id, forward_type, local_port, local_host, remote_host, remote_port, always_start) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                input.session_id,
                input.forward_type,
                input.local_port as i32,
                input.local_host,
                input.remote_host,
                input.remote_port.map(|v| v as i32),
                if input.always_start { 1 } else { 0 },
            ],
        )
        .map_err(|e| format!("Failed to create port forward: {}", e))?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.get_port_forward(id)
    }

    pub fn get_port_forward(&self, id: i64) -> Result<PortForward, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            &format!("SELECT {PORT_FORWARD_COLUMNS} FROM port_forwards WHERE id = ?1"),
            params![id],
            port_forward_from_row,
        )
        .map_err(|e| e.to_string())
    }

    pub fn update_port_forward(
        &self,
        id: i64,
        input: UpdatePortForwardInput,
    ) -> Result<PortForward, String> {
        let existing = self.get_port_forward(id)?;
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE port_forwards SET forward_type=?1, local_port=?2, local_host=?3, remote_host=?4, remote_port=?5, always_start=?6 WHERE id=?7",
            params![
                input.forward_type.unwrap_or(existing.forward_type),
                input.local_port.unwrap_or(existing.local_port) as i32,
                input.local_host.unwrap_or(existing.local_host),
                input.remote_host.unwrap_or(existing.remote_host),
                input.remote_port
                    .unwrap_or(existing.remote_port)
                    .map(|v| v as i32),
                input.always_start.map(|v| if v { 1 } else { 0 }).unwrap_or(if existing.always_start { 1 } else { 0 }),
                id,
            ],
        )
        .map_err(|e| format!("Failed to update port forward: {}", e))?;
        drop(conn);
        self.get_port_forward(id)
    }

    pub fn delete_port_forward(&self, id: i64) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM port_forwards WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SshConfigEntry {
    pub host: String,
    pub host_name: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<String>,
}
