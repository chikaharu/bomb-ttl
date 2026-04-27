//! On-disk state for idempotent re-scans.
//!
//! The state lives at `<root>/.bomb-ttl/state.json` (the `.bomb-ttl/`
//! subdirectory is always excluded from sweeping). Keys are the
//! `(inode, created_at_secs)` pair so a path that is deleted and
//! recreated with the same name does not collide with a stale entry.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub entries: BTreeMap<String, StateEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateEntry {
    pub path: String,
    pub jobid: String,
    pub created_at_secs: u64,
    pub delete_at_secs: u64,
    pub inode: u64,
}

impl State {
    pub const VERSION: u32 = 1;

    pub fn key(inode: u64, created_at_secs: u64) -> String {
        format!("{inode}:{created_at_secs}")
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(mut s) => {
                    if s.version == 0 {
                        s.version = Self::VERSION;
                    }
                    Ok(s)
                }
                Err(e) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State {
                version: Self::VERSION,
                entries: BTreeMap::new(),
            }),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Drop entries whose backing path no longer exists. Returns the
    /// number of orphans removed.
    pub fn prune_orphans(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, e| Path::new(&e.path).exists());
        before - self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_json() {
        let mut s = State {
            version: State::VERSION,
            entries: BTreeMap::new(),
        };
        s.entries.insert(
            State::key(42, 1_000),
            StateEntry {
                path: "/x/y".into(),
                jobid: "127.0.0.1:5000".into(),
                created_at_secs: 1_000,
                delete_at_secs: 2_000,
                inode: 42,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.json");
        s.save(&p).unwrap();
        let loaded = State::load(&p).unwrap();
        assert_eq!(loaded.version, State::VERSION);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(
            loaded.entries.get(&State::key(42, 1_000)).unwrap().jobid,
            "127.0.0.1:5000"
        );
    }

    #[test]
    fn missing_file_is_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.json");
        let s = State::load(&p).unwrap();
        assert!(s.entries.is_empty());
        assert_eq!(s.version, State::VERSION);
    }

    #[test]
    fn prune_orphans_removes_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let alive = dir.path().join("alive");
        std::fs::write(&alive, b"x").unwrap();
        let mut s = State {
            version: State::VERSION,
            ..State::default()
        };
        s.entries.insert(
            "1:1".into(),
            StateEntry {
                path: alive.to_string_lossy().into_owned(),
                jobid: "j1".into(),
                created_at_secs: 1,
                delete_at_secs: 2,
                inode: 1,
            },
        );
        s.entries.insert(
            "2:2".into(),
            StateEntry {
                path: dir.path().join("ghost").to_string_lossy().into_owned(),
                jobid: "j2".into(),
                created_at_secs: 2,
                delete_at_secs: 3,
                inode: 2,
            },
        );
        let removed = s.prune_orphans();
        assert_eq!(removed, 1);
        assert_eq!(s.entries.len(), 1);
    }
}
