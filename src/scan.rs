//! TTL scanner: walk one directory level under `<root>`, deleting
//! anything past its TTL on the spot and queueing a `qsub`-backed
//! delayed `rm` for everything else.

use crate::qsub::{submit_delayed_rm, QsubError};
use crate::state::{State, StateEntry};
use crate::{STATE_DIR_NAME, STATE_FILE_NAME};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Default, Clone)]
pub struct ScanStats {
    pub seen: u32,
    pub deleted_now: u32,
    pub already_scheduled: u32,
    pub newly_scheduled: u32,
    pub orphans_pruned: u32,
    pub errors: u32,
}

#[derive(Debug)]
pub struct ScanReport {
    pub stats: ScanStats,
    pub events: Vec<ScanEvent>,
}

#[derive(Debug)]
pub enum ScanEvent {
    DeletedNow {
        path: PathBuf,
        age: Duration,
    },
    Scheduled {
        path: PathBuf,
        jobid: String,
        delay: Duration,
    },
    AlreadyScheduled {
        path: PathBuf,
        jobid: String,
    },
    Error {
        path: PathBuf,
        message: String,
    },
}

pub fn scan_root(root: &Path, ttl: Duration, now: SystemTime) -> ScanReport {
    let mut report = ScanReport {
        stats: ScanStats::default(),
        events: Vec::new(),
    };

    let state_dir = root.join(STATE_DIR_NAME);
    let state_file = state_dir.join(STATE_FILE_NAME);
    let mut state = match State::load(&state_file) {
        Ok(s) => s,
        Err(e) => {
            report.stats.errors += 1;
            report.events.push(ScanEvent::Error {
                path: state_file.clone(),
                message: format!("load state: {e}"),
            });
            State {
                version: State::VERSION,
                entries: Default::default(),
            }
        }
    };
    let pruned = state.prune_orphans();
    report.stats.orphans_pruned = pruned as u32;

    let entries = match std::fs::read_dir(root) {
        Ok(it) => it,
        Err(e) => {
            report.stats.errors += 1;
            report.events.push(ScanEvent::Error {
                path: root.to_path_buf(),
                message: format!("read_dir: {e}"),
            });
            return report;
        }
    };

    for ent in entries.flatten() {
        let path = ent.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name == STATE_DIR_NAME {
            continue;
        }
        if name == ".gitignore" || name == "README.md" {
            continue;
        }
        report.stats.seen += 1;

        let meta = match ent.metadata() {
            Ok(m) => m,
            Err(e) => {
                report.stats.errors += 1;
                report.events.push(ScanEvent::Error {
                    path: path.clone(),
                    message: format!("stat: {e}"),
                });
                continue;
            }
        };

        let created = meta
            .created()
            .or_else(|_| meta.modified())
            .unwrap_or(SystemTime::now());
        let created_secs = created
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let inode = meta.ino();
        let age = now.duration_since(created).unwrap_or(Duration::ZERO);

        if age >= ttl {
            let r = if meta.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match r {
                Ok(_) => {
                    report.stats.deleted_now += 1;
                    report.events.push(ScanEvent::DeletedNow {
                        path: path.clone(),
                        age,
                    });
                    state.entries.remove(&State::key(inode, created_secs));
                }
                Err(e) => {
                    report.stats.errors += 1;
                    report.events.push(ScanEvent::Error {
                        path: path.clone(),
                        message: format!("rm: {e}"),
                    });
                }
            }
            continue;
        }

        let key = State::key(inode, created_secs);
        if let Some(e) = state.entries.get(&key) {
            report.stats.already_scheduled += 1;
            report.events.push(ScanEvent::AlreadyScheduled {
                path: path.clone(),
                jobid: e.jobid.clone(),
            });
            continue;
        }

        let delay = ttl - age;
        match submit_delayed_rm(&path, delay) {
            Ok(out) => {
                let delete_at_secs = created_secs.saturating_add(ttl.as_secs());
                state.entries.insert(
                    key,
                    StateEntry {
                        path: path.to_string_lossy().into_owned(),
                        jobid: out.jobid.clone(),
                        created_at_secs: created_secs,
                        delete_at_secs,
                        inode,
                    },
                );
                report.stats.newly_scheduled += 1;
                report.events.push(ScanEvent::Scheduled {
                    path: path.clone(),
                    jobid: out.jobid,
                    delay,
                });
            }
            Err(QsubError::NotFound) => {
                report.stats.errors += 1;
                report.events.push(ScanEvent::Error {
                    path: path.clone(),
                    message: "qsub not on PATH — `source <tren-crc>/scheduler/env.sh` first".into(),
                });
            }
            Err(e) => {
                report.stats.errors += 1;
                report.events.push(ScanEvent::Error {
                    path: path.clone(),
                    message: format!("submit qsub: {e}"),
                });
            }
        }
    }

    if let Err(e) = state.save(&state_file) {
        report.stats.errors += 1;
        report.events.push(ScanEvent::Error {
            path: state_file,
            message: format!("save state: {e}"),
        });
    }

    report
}
