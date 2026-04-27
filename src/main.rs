//! `bomb-ttl` CLI entry point.

use bomb_ttl::scan::ScanEvent;
use bomb_ttl::state::State;
use bomb_ttl::{
    ensure_root, qsub, resolve_root, scan, DEFAULT_INTERVAL_SEC, DEFAULT_TTL_MIN, STATE_DIR_NAME,
    STATE_FILE_NAME,
};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

/// TTL-based sweeper for `Workspace/tmp/`.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Override swept root (default: $WORKSPACE_TMP, else `<cwd>/Workspace/tmp/`).
    #[arg(long, global = true)]
    root: Option<PathBuf>,

    /// TTL in minutes (default: 1440 = 24h, env: BOMB_TTL_MIN).
    #[arg(long, global = true)]
    ttl: Option<u64>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Scan once: delete past-TTL entries on the spot, queue `qsub` for the rest.
    Scan,
    /// Loop `scan` every `--interval` seconds (default 60, env: BOMB_TTL_INTERVAL_SEC).
    Daemon {
        #[arg(long)]
        interval: Option<u64>,
    },
    /// Print currently-scheduled entries (path, jobid, delete-at).
    List,
    /// Cancel the queued `qsub` job and drop the state entry for `<path>`.
    Cancel { path: PathBuf },
}

fn ttl_from(cli: &Cli) -> Duration {
    let mins = cli
        .ttl
        .or_else(|| {
            std::env::var("BOMB_TTL_MIN")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(DEFAULT_TTL_MIN);
    Duration::from_secs(mins * 60)
}

fn interval_from(opt: Option<u64>) -> Duration {
    let secs = opt
        .or_else(|| {
            std::env::var("BOMB_TTL_INTERVAL_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(DEFAULT_INTERVAL_SEC);
    Duration::from_secs(secs.max(1))
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let root = resolve_root(cli.root.clone());
    if let Err(e) = ensure_root(&root) {
        eprintln!("bomb-ttl: ensure_root({}): {}", root.display(), e);
        return std::process::ExitCode::FAILURE;
    }
    let ttl = ttl_from(&cli);

    match cli.cmd {
        Cmd::Scan => run_scan(&root, ttl),
        Cmd::Daemon { interval } => run_daemon(&root, ttl, interval_from(interval)),
        Cmd::List => run_list(&root),
        Cmd::Cancel { ref path } => run_cancel(&root, path),
    }
}

fn run_scan(root: &std::path::Path, ttl: Duration) -> std::process::ExitCode {
    let report = scan::scan_root(root, ttl, SystemTime::now());
    print_report(&report);
    if report.stats.errors > 0 {
        std::process::ExitCode::from(1)
    } else {
        std::process::ExitCode::SUCCESS
    }
}

/// Daemon stop flag. Written to from the SIGTERM/SIGINT handler — must
/// be a plain `static AtomicBool` so the handler is async-signal-safe
/// (atomic stores compile to a single guaranteed-safe instruction).
static STOP_FLAG: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_term(_: libc::c_int) {
    // SAFETY: AtomicBool::store with Ordering::SeqCst is async-signal-safe
    // on every Tier-1 Rust target — it lowers to a single atomic write
    // with no allocator, mutex, or TLS access.
    STOP_FLAG.store(true, Ordering::SeqCst);
}

fn run_daemon(root: &std::path::Path, ttl: Duration, interval: Duration) -> std::process::ExitCode {
    if let Err(e) = unsafe { install_term_handler() } {
        eprintln!("bomb-ttl: warning: install_term_handler: {e}");
    }
    eprintln!(
        "bomb-ttl daemon: root={} ttl={}s interval={}s",
        root.display(),
        ttl.as_secs(),
        interval.as_secs()
    );
    while !STOP_FLAG.load(Ordering::SeqCst) {
        let report = scan::scan_root(root, ttl, SystemTime::now());
        print_report(&report);
        for _ in 0..interval.as_secs() {
            if STOP_FLAG.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    eprintln!("bomb-ttl daemon: shutdown");
    std::process::ExitCode::SUCCESS
}

fn run_list(root: &std::path::Path) -> std::process::ExitCode {
    let p = root.join(STATE_DIR_NAME).join(STATE_FILE_NAME);
    let s = match State::load(&p) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bomb-ttl: load state: {e}");
            return std::process::ExitCode::from(1);
        }
    };
    if s.entries.is_empty() {
        println!("(no scheduled entries)");
        return std::process::ExitCode::SUCCESS;
    }
    println!("path\tjobid\tdelete_at_unix\tdelete_at_iso");
    for e in s.entries.values() {
        println!(
            "{}\t{}\t{}\t{}",
            e.path,
            e.jobid,
            e.delete_at_secs,
            fmt_iso(e.delete_at_secs)
        );
    }
    std::process::ExitCode::SUCCESS
}

fn run_cancel(root: &std::path::Path, target: &std::path::Path) -> std::process::ExitCode {
    let p = root.join(STATE_DIR_NAME).join(STATE_FILE_NAME);
    let mut s = match State::load(&p) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bomb-ttl: load state: {e}");
            return std::process::ExitCode::from(1);
        }
    };
    let abs_target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let key = s
        .entries
        .iter()
        .find(|(_, v)| {
            std::path::Path::new(&v.path) == abs_target
                || std::path::Path::new(&v.path).file_name() == abs_target.file_name()
        })
        .map(|(k, _)| k.clone());
    let Some(key) = key else {
        eprintln!("bomb-ttl: no scheduled entry for {}", target.display());
        return std::process::ExitCode::from(2);
    };
    let entry = s.entries.remove(&key).unwrap();
    let workdir = root.parent().unwrap_or(root);
    match qsub::qdel(&entry.jobid, workdir) {
        Ok(true) => println!("cancelled jobid={} path={}", entry.jobid, entry.path),
        Ok(false) => println!(
            "qdel {} returned non-zero (entry dropped from state anyway)",
            entry.jobid
        ),
        Err(e) => eprintln!("bomb-ttl: qdel {}: {e}", entry.jobid),
    }
    if let Err(e) = s.save(&p) {
        eprintln!("bomb-ttl: save state: {e}");
        return std::process::ExitCode::from(1);
    }
    std::process::ExitCode::SUCCESS
}

fn print_report(report: &scan::ScanReport) {
    let s = &report.stats;
    eprintln!(
        "bomb-ttl scan: seen={} deleted_now={} scheduled+={} already={} pruned={} errors={}",
        s.seen, s.deleted_now, s.newly_scheduled, s.already_scheduled, s.orphans_pruned, s.errors
    );
    for ev in &report.events {
        match ev {
            ScanEvent::DeletedNow { path, age } => {
                eprintln!("  - deleted {} (age {}s)", path.display(), age.as_secs())
            }
            ScanEvent::Scheduled { path, jobid, delay } => eprintln!(
                "  + queued {} jobid={} delay={}s",
                path.display(),
                jobid,
                delay.as_secs()
            ),
            ScanEvent::AlreadyScheduled { path, jobid } => {
                eprintln!("  = already {} jobid={}", path.display(), jobid)
            }
            ScanEvent::Error { path, message } => {
                eprintln!("  ! error  {}: {}", path.display(), message)
            }
        }
    }
}

fn fmt_iso(secs: u64) -> String {
    let total = secs as i64;
    let days_from_epoch = total / 86_400;
    let day_secs = (total % 86_400) as u32;
    let h = day_secs / 3600;
    let m = (day_secs / 60) % 60;
    let s = day_secs % 60;
    let (y, mo, d) = civil_from_days(days_from_epoch);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

unsafe fn install_term_handler() -> std::io::Result<()> {
    let mut act: libc::sigaction = std::mem::zeroed();
    act.sa_sigaction = handle_term as *const () as usize;
    libc::sigemptyset(&mut act.sa_mask);
    let term = libc::sigaction(libc::SIGTERM, &act, std::ptr::null_mut());
    let intr = libc::sigaction(libc::SIGINT, &act, std::ptr::null_mut());
    if term != 0 || intr != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
