# bomb-ttl

> TTL-based sweeper for `Workspace/tmp/`, scheduled via
> [chikaharu/tren-crc](https://github.com/chikaharu/tren-crc).

[![CI](https://github.com/chikaharu/bomb-ttl/actions/workflows/ci.yml/badge.svg)](https://github.com/chikaharu/bomb-ttl/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE-APACHE)

`bomb-ttl` walks `Workspace/tmp/` (one level deep), deletes anything past
its TTL on the spot, and queues a `qsub`-backed delayed `rm -rf` for
everything else. The scheduler is the
[`tren-crc`](https://github.com/chikaharu/tren-crc) `qsub` binary, so the
queue is **PWD-local** (no central daemon, no root) and visible to the
rest of the tren ecosystem (`qstat`, `qwait`, `qdel`).

The point: stop using `/tmp/` as a scratch area (it gets cleared between
sessions on Replit, and is global) and switch to `Workspace/tmp/` as a
**first-class scratch directory whose contents will silently disappear
after a configurable TTL**.

## Why not just use one of the standard tools?

| Tool | Available on Replit? | Per-file TTL? | Survives session restart? | Verdict |
|---|---|---|---|---|
| `find Workspace/tmp -mtime +1 -delete` (cron) | ❌ no `cron` / no `systemd-timer` | yes (mtime) | n/a | unavailable |
| `tmpreaper` / `tmpwatch` | ❌ not packaged | yes | requires cron | unavailable |
| `systemd-tmpfiles` | ❌ no systemd-user | yes | requires systemd | unavailable |
| `systemd-run --on-active=24h …` | ❌ no systemd | yes (per invocation) | requires systemd | unavailable |
| `at now + 24 hours <<< rm -rf path` | ❌ no `atd` | yes | requires atd | unavailable |
| `sleep N && rm -rf path &` (bash background) | ✅ but loses on session exit | yes | **no** | won't fire after Replit kills the shell |
| **`bomb-ttl` + `tren-crc` qsub** | ✅ user-space | yes | yes (state file + qsub workdir) | this repo |

`tren-crc`'s `qsub` keeps job state in a PWD-local `.tren-<uuid>/`
directory that survives shell exit; combined with bomb-ttl's
`<root>/.bomb-ttl/state.json` for idempotency, the deletion fires even
after the originating shell or daemon has gone away.

## Install

`bomb-ttl` shells out to `qsub` / `qdel` from `tren-crc`, so install
that first:

```sh
git clone https://github.com/chikaharu/tren-crc /tmp/tren-crc
cd /tmp/tren-crc/artifacts/bitrag/scheduler
source ./env.sh    # builds tren-crc on first source, puts qsub on PATH
```

Then:

```sh
cargo install --git https://github.com/chikaharu/bomb-ttl
```

## Usage

```sh
# one-shot scan of $WORKSPACE_TMP (or `./Workspace/tmp/` if unset)
bomb-ttl scan

# loop scan every 60s
bomb-ttl daemon

# loop scan every 5 minutes, custom TTL of 2 hours
bomb-ttl daemon --interval 300 --ttl 120

# show currently scheduled deletions
bomb-ttl list

# cancel a scheduled deletion
bomb-ttl cancel ./Workspace/tmp/big-build-output
```

### Configuration

| Flag / env | Default | Meaning |
|---|---|---|
| `--root <dir>` | `$WORKSPACE_TMP` ⊃ `<cwd>/Workspace/tmp/` | Directory to sweep. |
| `--ttl <minutes>` / `BOMB_TTL_MIN` | 1440 (24 h) | Time after creation before deletion. |
| `--interval <sec>` / `BOMB_TTL_INTERVAL_SEC` | 60 | `daemon` rescan period. |
| `BOMB_TTL_QSUB_BIN` | `qsub` | Override the `qsub` binary path (used by tests). |
| `BOMB_TTL_QDEL_BIN` | `qdel` | Override the `qdel` binary path. |

### State file

`bomb-ttl` keeps an idempotency record at
`<root>/.bomb-ttl/state.json`. Keys are the `(inode, created_at_secs)`
pair, so:

- the same path is never re-queued twice across re-scans
- a path that gets deleted and re-created with the same name gets a
  fresh schedule (different inode and/or created_at)
- entries whose backing path no longer exists are pruned each scan
  (orphan cleanup)

`<root>/.bomb-ttl/` itself is excluded from sweeping, as are
`.gitignore` and `README.md` placed by `bomb-ttl` when it first
provisions an empty root.

## Known constraints

- **PWD-local scheduler**: each `qsub` invocation creates (or reuses) a
  `.tren-<uuid>/` directory in the cwd at submit time. Run `bomb-ttl`
  always from the same parent of `Workspace/tmp/` so the same tren
  workdir is reused; otherwise your `qstat` / `qdel` won't see jobs
  submitted from a different cwd.
- **Daemon shutdown**: `bomb-ttl daemon` reacts to `SIGTERM` / `SIGINT`
  by setting a stop flag and exiting after the current scan
  completes. Because state is flushed at the end of every scan, no
  scheduled job is lost; only newly-arrived files in the in-flight
  scan need to wait for the next daemon to come up.
- **Survival across daemon restart**: scheduled `qsub` jobs continue
  to run even after `bomb-ttl` exits (they were spawned in their own
  process group). On the next `bomb-ttl scan`, the state file is
  read back and surviving entries are reported as `already scheduled`.
- **Out of scope (today)**: per-path manual scheduling
  (`bomb-ttl schedule <path> <duration>`), exclude patterns beyond
  the hard-coded `.bomb-ttl/` / `.gitignore` / `README.md`,
  publishing to crates.io, registering bomb-ttl as a Replit
  artifact / workflow.

## License

Apache-2.0. See [LICENSE-APACHE](./LICENSE-APACHE).
