//! Orphaned-child reaping (37 §2.2).
//!
//! ffmpeg is spawned as a child process for decode, PCM, encode and proxy jobs.
//! If the editor is `SIGKILL`ed (or crashes hard) its `Drop`-based kill paths
//! never run, and those ffmpeg children are reparented to init and keep running
//! — burning CPU and holding file handles. This module gives every child a
//! paper trail so a later launch can reap the strays.
//!
//! **Tier 1 (portable, this module):** each running editor owns a registry file
//! `<cache_dir>/photonic-children-<pid>.json` listing its live children. On
//! startup [`ChildRegistry::reap_orphans`] scans every registry file whose
//! owning editor pid is gone, kills each recorded child that is still ours, and
//! deletes the file.
//!
//! **The pid-reuse guard is mandatory.** A recorded pid may have been recycled
//! by the OS onto an unrelated process. We record wall-clock spawn time in each
//! [`ChildRecord`] and, before killing, compare it to the live process's actual
//! start time: if the live process started *after* we recorded the child, it is
//! a different process on a reused pid and MUST be spared. When the platform
//! start-time probe is unavailable we skip the kill rather than risk it.
//!
//! **Tier 2 (platform):** Linux additionally arms `PR_SET_PDEATHSIG(SIGKILL)`
//! via `pre_exec` so the kernel kills the child the instant the parent dies;
//! Windows uses a kill-on-close Job Object; macOS relies on tier 1 only.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One recorded child process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildRecord {
    /// OS process id.
    pub pid: u32,
    /// Wall-clock time we recorded the spawn, in Unix seconds. The pid-reuse
    /// guard compares this against the live process's observed start time.
    pub started_unix: u64,
    /// Which subsystem spawned it: `"decode" | "pcm" | "encode" | "proxy"`.
    /// Owned so the record round-trips through serde (a borrowed `&'static str`
    /// cannot be deserialized from a temporary buffer); constructed from a
    /// `&'static str` at every call site via [`record_for`].
    pub kind: String,
}

/// Small slack (seconds) applied to the pid-reuse comparison: we record a child
/// a hair *after* the OS actually started it, so the true child's observed
/// start time is at or slightly before `started_unix`. Anything meaningfully
/// newer is a reused pid.
const START_TIME_SLACK_SECS: u64 = 5;

/// True when a live pid whose recorded spawn time was `recorded_started_unix`
/// and whose currently-observed process start time is `observed_started_unix`
/// is a **reused pid** — i.e. an unrelated process that must NOT be killed.
///
/// The true child started at or before we recorded it, so `observed <=
/// recorded + slack`. A reused pid started later, so `observed > recorded +
/// slack`.
fn is_reused_pid(recorded_started_unix: u64, observed_started_unix: u64) -> bool {
    observed_started_unix > recorded_started_unix.saturating_add(START_TIME_SLACK_SECS)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build a [`ChildRecord`] for a just-spawned child of `kind` with the current
/// wall-clock spawn time.
pub fn record_for(pid: u32, kind: &'static str) -> ChildRecord {
    ChildRecord {
        pid,
        started_unix: now_unix(),
        kind: kind.to_string(),
    }
}

fn registry_path(cache_dir: &Path, pid: u32) -> PathBuf {
    cache_dir.join(format!("photonic-children-{pid}.json"))
}

/// Parse the pid out of a `photonic-children-<pid>.json` file name.
fn pid_from_registry_name(name: &str) -> Option<u32> {
    name.strip_prefix("photonic-children-")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

/// The per-process registry of live ffmpeg children.
pub struct ChildRegistry {
    path: PathBuf,
}

impl ChildRegistry {
    /// Open (create) this process's registry file under `cache_dir`.
    pub fn open(cache_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(cache_dir)?;
        let path = registry_path(cache_dir, std::process::id());
        if !path.exists() {
            super::atomic_write::write_atomic(&path, b"[]")?;
        }
        Ok(Self { path })
    }

    fn read(&self) -> Vec<ChildRecord> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write(&self, records: &[ChildRecord]) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(records).map_err(std::io::Error::other)?;
        super::atomic_write::write_atomic(&self.path, &bytes)
    }

    /// Append a child record (atomic rewrite).
    pub fn record(&self, rec: ChildRecord) -> std::io::Result<()> {
        let mut records = self.read();
        records.push(rec);
        self.write(&records)
    }

    /// Drop a child record by pid (atomic rewrite). No error if absent.
    pub fn forget(&self, pid: u32) -> std::io::Result<()> {
        let mut records = self.read();
        records.retain(|r| r.pid != pid);
        self.write(&records)
    }

    /// Scan every registry file under `cache_dir`. For each file whose owning
    /// editor pid is gone, kill each recorded child that is still alive and is
    /// still ours (pid-reuse guarded), then delete the file. Returns the number
    /// of children reaped. Never fails the launch — unreadable entries are
    /// skipped.
    pub fn reap_orphans(cache_dir: &Path) -> usize {
        let our_pid = std::process::id();
        let entries = match std::fs::read_dir(cache_dir) {
            Ok(e) => e,
            Err(_) => return 0,
        };
        let mut reaped = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(owner_pid) = pid_from_registry_name(name) else {
                continue;
            };
            // Our own registry, or a still-running editor's, is left alone.
            if owner_pid == our_pid || pid_alive(owner_pid) {
                continue;
            }
            let records: Vec<ChildRecord> = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            for rec in records {
                if !pid_alive(rec.pid) {
                    continue;
                }
                // Pid-reuse guard: only kill when we can positively confirm the
                // live process is not newer than our record. Unknown => skip.
                match process_start_unix(rec.pid) {
                    Some(observed) if is_reused_pid(rec.started_unix, observed) => continue,
                    Some(_) => {
                        if kill_pid(rec.pid) {
                            reaped += 1;
                        }
                    }
                    None => continue, // no probe => do not risk an unrelated kill
                }
            }
            let _ = std::fs::remove_file(&path);
        }
        reaped
    }
}

// ---- platform process probes ------------------------------------------------

/// Whether a pid currently refers to a live process.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // `kill(pid, 0)` returns 0 if we may signal it, ESRCH if it is gone.
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// Send an unconditional kill to `pid`. Returns true on apparent success.
#[cfg(unix)]
fn kill_pid(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) == 0 }
}

#[cfg(not(unix))]
fn kill_pid(_pid: u32) -> bool {
    false
}

/// Observe a live process's start time in Unix seconds, or `None` when the
/// platform probe is unavailable (in which case reaping must skip the kill).
///
/// Linux: `/proc/<pid>/stat` field 22 (`starttime`) is in clock ticks since
/// boot; convert to wall clock via boot time from `/proc/stat`'s `btime`.
#[cfg(target_os = "linux")]
fn process_start_unix(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 22 is starttime, but fields 2 (comm) can contain spaces inside
    // parentheses; split after the trailing ')'.
    let close = stat.rfind(')')?;
    let rest = &stat[close + 2..];
    let starttime_ticks: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;

    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clk_tck <= 0 {
        return None;
    }
    let btime = proc_btime()?;
    Some(btime + starttime_ticks / (clk_tck as u64))
}

/// Boot time (Unix seconds) from `/proc/stat`'s `btime` line.
#[cfg(target_os = "linux")]
fn proc_btime() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    for line in stat.lines() {
        if let Some(v) = line.strip_prefix("btime ") {
            return v.trim().parse().ok();
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn process_start_unix(_pid: u32) -> Option<u64> {
    // Windows: GetProcessTimes; macOS: proc_pidinfo. Not yet wired — returning
    // None makes reaping conservatively skip the kill on those platforms.
    None
}

/// Arm parent-death signalling on a `Command` so the child is killed the moment
/// this process dies. Linux-only (`PR_SET_PDEATHSIG`); a no-op elsewhere. Call
/// before `.spawn()` on every ffmpeg child.
#[cfg(target_os = "linux")]
pub fn arm_parent_death_signal(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `pre_exec` runs in the forked child before exec. `prctl` is
    // async-signal-safe and touches no allocator/lock state.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub fn arm_parent_death_signal(_command: &mut std::process::Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_record_serde_round_trips() {
        let rec = ChildRecord {
            pid: 4242,
            started_unix: 1_700_000_000,
            kind: "decode".to_string(),
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: ChildRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn registry_name_pid_round_trips() {
        assert_eq!(
            pid_from_registry_name("photonic-children-99.json"),
            Some(99)
        );
        assert_eq!(pid_from_registry_name("photonic-children-.json"), None);
        assert_eq!(pid_from_registry_name("something-else.json"), None);
    }

    #[test]
    fn reused_pid_is_spared() {
        // Recorded at t=1000. A live process that actually started well after
        // (t=2000) is a reused pid and must be spared.
        assert!(is_reused_pid(1000, 2000));
        // The true child started at or just before we recorded it.
        assert!(!is_reused_pid(1000, 1000));
        assert!(!is_reused_pid(1000, 998));
        // Within the slack window: still treated as ours.
        assert!(!is_reused_pid(1000, 1000 + START_TIME_SLACK_SECS));
        assert!(is_reused_pid(1000, 1000 + START_TIME_SLACK_SECS + 1));
    }

    #[test]
    fn record_and_forget_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "photonic-childreg-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = ChildRegistry::open(&dir).unwrap();
        reg.record(record_for(11, "decode")).unwrap();
        reg.record(record_for(22, "encode")).unwrap();
        assert_eq!(reg.read().len(), 2);
        reg.forget(11).unwrap();
        let after = reg.read();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].pid, 22);
        std::fs::remove_dir_all(&dir).ok();
    }
}
