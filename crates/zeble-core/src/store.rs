//! On-disk storage: the saved connection list and each connection's query tabs.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::config::{ConnectionConfig, new_id};

/// The app's data folder, e.g. `~/Library/Application Support/zeble` on macOS.
///
/// `ZEBLE_DATA_DIR` overrides it (used by tests and screenshots).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZEBLE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("zeble")
}

/// Write `contents` atomically with owner-only permissions.
fn write_private(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

#[derive(Serialize, Deserialize, Default)]
struct ConnectionsFile {
    version: u32,
    connections: Vec<ConnectionConfig>,
}

/// The ordered list of saved connections, persisted to `connections.json`.
pub struct ConnectionStore {
    path: PathBuf,
    connections: Vec<ConnectionConfig>,
}

impl ConnectionStore {
    /// Load from the default location.
    pub fn load_default() -> anyhow::Result<Self> {
        Self::load(data_dir().join("connections.json"))
    }

    /// Load from `path`; a missing file is an empty list.
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let connections = match fs::read(&path) {
            Ok(bytes) => {
                let file: ConnectionsFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("reading {}", path.display()))?;
                file.connections
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { path, connections })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connections(&self) -> &[ConnectionConfig] {
        &self.connections
    }

    pub fn get(&self, id: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.id == id)
    }

    fn save(&self) -> anyhow::Result<()> {
        let file = ConnectionsFile {
            version: 1,
            connections: self.connections.clone(),
        };
        write_private(&self.path, &serde_json::to_vec_pretty(&file)?)
    }

    /// Insert a new connection or replace the one with the same id.
    pub fn upsert(&mut self, config: ConnectionConfig) -> anyhow::Result<()> {
        match self.connections.iter_mut().find(|c| c.id == config.id) {
            Some(existing) => *existing = config,
            None => self.connections.push(config),
        }
        self.save()
    }

    /// Copy a connection, placing the copy right after the original.
    pub fn duplicate(&mut self, id: &str) -> anyhow::Result<Option<ConnectionConfig>> {
        let Some(ix) = self.connections.iter().position(|c| c.id == id) else {
            return Ok(None);
        };
        let mut copy = self.connections[ix].clone();
        copy.id = new_id();
        copy.name = format!("{} copy", copy.display_name());
        self.connections.insert(ix + 1, copy.clone());
        self.save()?;
        Ok(Some(copy))
    }

    pub fn delete(&mut self, id: &str) -> anyhow::Result<()> {
        self.connections.retain(|c| c.id != id);
        self.save()?;
        // Saved query tabs belong to the connection; drop them too.
        let _ = fs::remove_file(QueryTabsFile::path_for(&self.path, id));
        Ok(())
    }

    /// Move the connection at `from` so it ends up at index `to`.
    pub fn move_to(&mut self, from: usize, to: usize) -> anyhow::Result<()> {
        if from >= self.connections.len() || from == to {
            return Ok(());
        }
        let item = self.connections.remove(from);
        let to = to.min(self.connections.len());
        self.connections.insert(to, item);
        self.save()
    }
}

/// A query tab's saved title and SQL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub title: String,
    pub sql: String,
}

/// The query tabs saved for one connection, in `queries/<connection id>.json`.
#[derive(Serialize, Deserialize, Default)]
pub struct QueryTabsFile {
    pub tabs: Vec<SavedQuery>,
}

impl QueryTabsFile {
    fn path_for(connections_path: &Path, id: &str) -> PathBuf {
        connections_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("queries")
            .join(format!("{id}.json"))
    }

    fn default_path(id: &str) -> PathBuf {
        Self::path_for(&data_dir().join("connections.json"), id)
    }

    pub fn load(id: &str) -> Vec<SavedQuery> {
        fs::read(Self::default_path(id))
            .ok()
            .and_then(|b| serde_json::from_slice::<QueryTabsFile>(&b).ok())
            .map(|f| f.tabs)
            .unwrap_or_default()
    }

    pub fn save(id: &str, tabs: Vec<SavedQuery>) -> anyhow::Result<()> {
        let file = QueryTabsFile { tabs };
        write_private(&Self::default_path(id), &serde_json::to_vec_pretty(&file)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(name: &str) -> ConnectionConfig {
        ConnectionConfig {
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn crud_and_reorder_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let mut store = ConnectionStore::load(path.clone()).unwrap();
        assert!(store.connections().is_empty());

        let (a, b, c) = (conn("a"), conn("b"), conn("c"));
        store.upsert(a.clone()).unwrap();
        store.upsert(b.clone()).unwrap();
        store.upsert(c.clone()).unwrap();
        store.move_to(2, 0).unwrap();
        let mut edited = b.clone();
        edited.name = "b2".into();
        store.upsert(edited).unwrap();
        let copy = store.duplicate(&a.id).unwrap().unwrap();
        store.delete(&c.id).unwrap();

        let reloaded = ConnectionStore::load(path.clone()).unwrap();
        let names: Vec<_> = reloaded
            .connections()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, ["a", "a copy", "b2"]);
        assert_ne!(copy.id, a.id);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
