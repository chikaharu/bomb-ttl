//! `bomb-ttl` — TTL-based sweeper for `Workspace/tmp/` that delegates
//! delayed deletion to the [`tren-crc`](https://github.com/chikaharu/tren-crc)
//! `qsub` job scheduler.
//!
//! The CLI is in `src/main.rs`; the public surface here is the building
//! blocks the binary glues together.

pub mod qsub;
pub mod scan;
pub mod state;

pub use qsub::{submit_delayed_rm, QsubError, QsubOutcome};
pub use scan::{scan_root, ScanReport, ScanStats};
pub use state::{State, StateEntry};

/// Default TTL: 24 hours, in minutes.
pub const DEFAULT_TTL_MIN: u64 = 1440;

/// Default daemon scan interval, in seconds.
pub const DEFAULT_INTERVAL_SEC: u64 = 60;

/// Subdirectory inside the swept root that holds the bomb-ttl state.
/// Always excluded from sweeping.
pub const STATE_DIR_NAME: &str = ".bomb-ttl";

/// State file name inside [`STATE_DIR_NAME`].
pub const STATE_FILE_NAME: &str = "state.json";

/// Resolve the swept root: explicit `--root` overrides everything, else
/// `$WORKSPACE_TMP`, else `<cwd>/Workspace/tmp/`.
pub fn resolve_root(explicit: Option<std::path::PathBuf>) -> std::path::PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    if let Ok(env) = std::env::var("WORKSPACE_TMP") {
        if !env.is_empty() {
            return std::path::PathBuf::from(env);
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("Workspace")
        .join("tmp")
}

/// Ensure the swept root exists, with a `.gitignore` (`*` + `!.gitignore`)
/// and a `README.md` describing the directory's role.
pub fn ensure_root(root: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let gi = root.join(".gitignore");
    if !gi.exists() {
        std::fs::write(&gi, "*\n!.gitignore\n!README.md\n")?;
    }
    let rm = root.join("README.md");
    if !rm.exists() {
        std::fs::write(
            &rm,
            "# Workspace/tmp/\n\n\
             Primary scratch directory. Files placed here are swept by\n\
             [chikaharu/bomb-ttl](https://github.com/chikaharu/bomb-ttl)\n\
             after the configured TTL (default 24h) via the `tren-crc`\n\
             `qsub` scheduler.\n\n\
             Do **not** put long-lived sources, build outputs you want to\n\
             keep, or anything you can't reproduce here.\n",
        )?;
    }
    let sd = root.join(STATE_DIR_NAME);
    std::fs::create_dir_all(&sd)?;
    Ok(())
}
