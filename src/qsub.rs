//! Thin wrapper around the `tren-crc` `qsub` / `qdel` binaries.
//!
//! The job we submit is a `bash -c "sleep <delay> && rm -rf -- <path>"`
//! invocation. `qsub` from `tren-crc` blocks until its job finishes (it
//! prints `[qsub] exit=N` at the end), so we deliberately let the spawned
//! `qsub` process run detached after we've parsed its job address from
//! stderr — when `bomb-ttl` exits, the orphaned `qsub` child is reparented
//! to PID 1 and continues sleeping.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Outcome of [`submit_delayed_rm`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QsubOutcome {
    /// Job address as reported on the `[qsub] node <addr>` line.
    pub jobid: String,
    /// Workdir as reported on the same line, if present.
    pub workdir: Option<String>,
}

#[derive(Debug)]
pub enum QsubError {
    /// `qsub` is not on `PATH` — caller likely forgot
    /// `source <tren-crc>/scheduler/env.sh`.
    NotFound,
    /// `qsub` was found but exited / closed stderr before printing the
    /// `[qsub] node …` line we expected.
    NoJobAddress,
    /// I/O error talking to the spawned process.
    Io(std::io::Error),
}

impl std::fmt::Display for QsubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QsubError::NotFound => write!(
                f,
                "qsub not found on PATH — did you `source <tren-crc>/scheduler/env.sh`?"
            ),
            QsubError::NoJobAddress => {
                write!(f, "qsub closed before printing `[qsub] node …` job address")
            }
            QsubError::Io(e) => write!(f, "qsub io: {e}"),
        }
    }
}

impl std::error::Error for QsubError {}

impl From<std::io::Error> for QsubError {
    fn from(e: std::io::Error) -> Self {
        QsubError::Io(e)
    }
}

/// Submit `bash -c "sleep <delay> && rm -rf -- <path>"` via `qsub` and
/// return the job address as soon as we see the `[qsub] node …` line on
/// stderr. The spawned `qsub` is left running detached.
pub fn submit_delayed_rm(target: &Path, delay: Duration) -> Result<QsubOutcome, QsubError> {
    submit_delayed_rm_with_bin(
        std::env::var("BOMB_TTL_QSUB_BIN").unwrap_or_else(|_| "qsub".into()),
        target,
        delay,
    )
}

pub fn submit_delayed_rm_with_bin(
    qsub_bin: String,
    target: &Path,
    delay: Duration,
) -> Result<QsubOutcome, QsubError> {
    let target_q = shell_single_quote(&target.to_string_lossy());
    let cmd = format!("sleep {} && rm -rf -- {}", delay.as_secs(), target_q,);
    let mut child = match Command::new(&qsub_bin)
        .arg("bash")
        .arg("-c")
        .arg(&cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(QsubError::NotFound),
        Err(e) => return Err(QsubError::Io(e)),
    };

    let stderr = child.stderr.take().ok_or(QsubError::NoJobAddress)?;
    let stdout = child.stdout.take();
    let mut found: Option<QsubOutcome> = None;
    {
        let mut r = BufReader::new(stderr);
        let mut buf = String::new();
        for _ in 0..16 {
            buf.clear();
            match r.read_line(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    if let Some(o) = parse_node_line(&buf) {
                        found = Some(o);
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    }
    if found.is_none() {
        if let Some(out) = stdout {
            let mut r = BufReader::new(out);
            let mut buf = String::new();
            for _ in 0..16 {
                buf.clear();
                match r.read_line(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if let Some(o) = parse_node_line(&buf) {
                            found = Some(o);
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
    drop(child);
    found.ok_or(QsubError::NoJobAddress)
}

/// Try to delete a queued job via `qdel <jobid>`. Returns `Ok(true)` if
/// `qdel` exited 0, `Ok(false)` if non-zero. `qdel` not on `PATH` is
/// reported as [`QsubError::NotFound`].
pub fn qdel(jobid: &str) -> Result<bool, QsubError> {
    let bin = std::env::var("BOMB_TTL_QDEL_BIN").unwrap_or_else(|_| "qdel".into());
    let st = match Command::new(&bin)
        .arg(jobid)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(QsubError::NotFound),
        Err(e) => return Err(QsubError::Io(e)),
    };
    Ok(st.success())
}

fn parse_node_line(line: &str) -> Option<QsubOutcome> {
    let l = line.trim_start();
    let rest = l.strip_prefix("[qsub] node ")?;
    let mut parts = rest.split_whitespace();
    let addr = parts.next()?.to_string();
    let workdir = if let Some(tag) = parts.next() {
        if tag == "workdir" {
            parts.next().map(|s| s.to_string())
        } else {
            None
        }
    } else {
        None
    };
    Some(QsubOutcome {
        jobid: addr,
        workdir,
    })
}

fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_classic_node_line() {
        let o = parse_node_line("[qsub] node 127.0.0.1:54321  workdir /x/.tren-abc\n").unwrap();
        assert_eq!(o.jobid, "127.0.0.1:54321");
        assert_eq!(o.workdir.as_deref(), Some("/x/.tren-abc"));
    }

    #[test]
    fn parses_addr_only() {
        let o = parse_node_line("[qsub] node 127.0.0.1:1\n").unwrap();
        assert_eq!(o.jobid, "127.0.0.1:1");
        assert_eq!(o.workdir, None);
    }

    #[test]
    fn ignores_unrelated_lines() {
        assert!(parse_node_line("[qsub] exit=0\n").is_none());
        assert!(parse_node_line("hello\n").is_none());
    }

    #[test]
    fn shell_quote_handles_apostrophe() {
        let q = shell_single_quote("a'b c");
        assert_eq!(q, "'a'\\''b c'");
    }
}
