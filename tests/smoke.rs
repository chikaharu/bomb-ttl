//! End-to-end smoke tests for `bomb_ttl::scan::scan_root`.
//!
//! We do not need the real `tren-crc` / `qsub` here: a tiny shell
//! fixture in `tests/fixtures/qsub_fake.sh` prints the
//! `[qsub] node <addr>` line that bomb-ttl looks for and then exits.
//! The `BOMB_TTL_QSUB_BIN` env var (read by `submit_delayed_rm`)
//! points the scanner at it.

use bomb_ttl::scan::{scan_root, ScanEvent};
use bomb_ttl::state::State;
use bomb_ttl::{ensure_root, STATE_DIR_NAME, STATE_FILE_NAME};
use std::time::{Duration, SystemTime};

fn fixture_qsub() -> String {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push("qsub_fake.sh");
    let mode = std::fs::metadata(&p).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    let mut new_mode = mode.clone();
    new_mode.set_mode(0o755);
    let _ = std::fs::set_permissions(&p, new_mode);
    p.to_string_lossy().into_owned()
}

#[test]
fn deletes_past_ttl_immediately_no_qsub_needed() {
    let root = tempfile::tempdir().unwrap();
    ensure_root(root.path()).unwrap();
    let stale = root.path().join("stale.txt");
    std::fs::write(&stale, b"x").unwrap();

    let now = SystemTime::now() + Duration::from_secs(120);
    let report = scan_root(root.path(), Duration::from_secs(60), now);

    assert!(!stale.exists(), "stale file should be deleted on the spot");
    assert_eq!(report.stats.deleted_now, 1);
    assert_eq!(report.stats.newly_scheduled, 0);
    assert!(matches!(report.events[0], ScanEvent::DeletedNow { .. }));
}

#[test]
fn schedules_future_entry_via_qsub_once_idempotent() {
    let qsub = fixture_qsub();
    std::env::set_var("BOMB_TTL_QSUB_BIN", &qsub);

    let root = tempfile::tempdir().unwrap();
    ensure_root(root.path()).unwrap();
    let fresh = root.path().join("fresh.txt");
    std::fs::write(&fresh, b"x").unwrap();

    let now = SystemTime::now();
    let r1 = scan_root(root.path(), Duration::from_secs(3600), now);
    assert_eq!(r1.stats.seen, 1, "events: {:#?}", r1.events);
    assert_eq!(r1.stats.newly_scheduled, 1, "events: {:#?}", r1.events);
    assert_eq!(r1.stats.deleted_now, 0);

    let state_path = root.path().join(STATE_DIR_NAME).join(STATE_FILE_NAME);
    let s = State::load(&state_path).unwrap();
    assert_eq!(s.entries.len(), 1);
    let entry = s.entries.values().next().unwrap();
    assert!(
        entry.jobid.starts_with("127.0.0.1:"),
        "jobid={}",
        entry.jobid
    );
    assert!(entry.delete_at_secs > 0);

    let r2 = scan_root(root.path(), Duration::from_secs(3600), now);
    assert_eq!(
        r2.stats.newly_scheduled, 0,
        "second scan must not re-queue: {:#?}",
        r2.events
    );
    assert_eq!(r2.stats.already_scheduled, 1);

    std::env::remove_var("BOMB_TTL_QSUB_BIN");
}

#[test]
fn skips_state_dir_and_meta_files() {
    let qsub = fixture_qsub();
    std::env::set_var("BOMB_TTL_QSUB_BIN", &qsub);

    let root = tempfile::tempdir().unwrap();
    ensure_root(root.path()).unwrap();
    // ensure_root creates README.md and .gitignore; they must NOT count.
    let r = scan_root(root.path(), Duration::from_secs(3600), SystemTime::now());
    assert_eq!(
        r.stats.seen, 0,
        "ensure_root meta files must be skipped: {:#?}",
        r.events
    );
    assert_eq!(r.stats.newly_scheduled, 0);

    std::env::remove_var("BOMB_TTL_QSUB_BIN");
}

#[test]
fn prunes_orphan_state_entries() {
    use std::collections::BTreeMap;
    let root = tempfile::tempdir().unwrap();
    ensure_root(root.path()).unwrap();
    let mut s = State {
        version: State::VERSION,
        entries: BTreeMap::new(),
    };
    s.entries.insert(
        "99:99".into(),
        bomb_ttl::state::StateEntry {
            path: root.path().join("ghost").to_string_lossy().into_owned(),
            jobid: "127.0.0.1:1".into(),
            created_at_secs: 1,
            delete_at_secs: 99_999_999_999,
            inode: 99,
        },
    );
    let state_path = root.path().join(STATE_DIR_NAME).join(STATE_FILE_NAME);
    s.save(&state_path).unwrap();

    let r = scan_root(root.path(), Duration::from_secs(3600), SystemTime::now());
    assert_eq!(r.stats.orphans_pruned, 1);
    let s2 = State::load(&state_path).unwrap();
    assert!(s2.entries.is_empty());
}
