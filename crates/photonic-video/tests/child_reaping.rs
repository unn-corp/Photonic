//! Orphaned-child reaping (37 §2.2) — the real-process cases.
//!
//! These spawn actual `sleep` children and manipulate registry files on disk,
//! so they are `#[ignore]`d by default (matching the real-subprocess
//! convention at `tests/session_playback.rs:663`). Run with
//! `cargo test -p photonic-video --test child_reaping -- --ignored`.
//!
//! The pure-logic pieces (serde, the pid-reuse predicate, record/forget) have
//! non-ignored unit tests inside `media::child_registry`.

#![cfg(unix)]

use std::path::Path;
use std::process::{Child, Command};
use std::time::{SystemTime, UNIX_EPOCH};

use photonic_video::media::child_registry::{ChildRecord, ChildRegistry};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "photonic-reap-{tag}-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Spawn a long-lived child we can look for after reaping.
fn spawn_sleeper() -> Child {
    Command::new("sleep")
        .arg("120")
        .spawn()
        .expect("spawn sleep")
}

fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Write a registry file naming `records`, owned by `owner_pid`.
fn write_registry(dir: &Path, owner_pid: u32, records: &[ChildRecord]) {
    let path = dir.join(format!("photonic-children-{owner_pid}.json"));
    std::fs::write(&path, serde_json::to_vec(records).unwrap()).unwrap();
}

/// A pid that is certainly dead: spawn `true` and reap it.
fn dead_pid() -> u32 {
    let mut c = Command::new("true").spawn().unwrap();
    let pid = c.id();
    let _ = c.wait();
    pid
}

#[test]
#[ignore = "spawns real processes; run with --ignored"]
fn reaps_live_child_of_dead_parent() {
    let dir = tmp_dir("live");
    let mut child = spawn_sleeper();
    let pid = child.id();
    // Recorded "just now", so its observed start time is not newer than the record.
    let rec = ChildRecord {
        pid,
        started_unix: now_unix(),
        kind: "decode".to_string(),
    };
    let dead_parent = dead_pid();
    write_registry(&dir, dead_parent, &[rec]);

    let reaped = ChildRegistry::reap_orphans(&dir);
    assert_eq!(reaped, 1, "the live orphan should be reaped");
    // The sleeper's real OS parent is *this* test process (a simulated dead
    // parent only exists in the registry file), so after SIGKILL it lingers as
    // a zombie until we wait() on it — and `kill(pid, 0)` reports a zombie as
    // "alive". Reap it and assert it was terminated by SIGKILL, which is the
    // actual guarantee: `reap_orphans` delivered the kill.
    use std::os::unix::process::ExitStatusExt;
    let status = child.wait().expect("wait on reaped child");
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "child must be terminated by SIGKILL after reaping (status: {status:?})"
    );
    assert!(
        !dir.join(format!("photonic-children-{dead_parent}.json"))
            .exists(),
        "registry file must be removed"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore = "spawns real processes; run with --ignored"]
fn reused_pid_is_not_killed() {
    let dir = tmp_dir("reuse");
    let mut child = spawn_sleeper();
    let pid = child.id();
    // Record a start time far in the PAST — the live pid actually started now,
    // so it looks like a reused pid and must be spared.
    let rec = ChildRecord {
        pid,
        started_unix: now_unix().saturating_sub(10_000),
        kind: "decode".to_string(),
    };
    let dead_parent = dead_pid();
    write_registry(&dir, dead_parent, &[rec]);

    let reaped = ChildRegistry::reap_orphans(&dir);
    assert_eq!(reaped, 0, "a reused pid must not be killed");
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(pid_alive(pid), "the unrelated live process must survive");
    child.kill().ok();
    let _ = child.wait();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore = "spawns real processes; run with --ignored"]
fn registry_of_live_owner_is_untouched() {
    let dir = tmp_dir("liveowner");
    let mut child = spawn_sleeper();
    let pid = child.id();
    let rec = ChildRecord {
        pid,
        started_unix: now_unix(),
        kind: "decode".to_string(),
    };
    // Owner is OUR pid — a live editor; its registry must be left alone.
    let owner = std::process::id();
    write_registry(&dir, owner, &[rec]);

    let reaped = ChildRegistry::reap_orphans(&dir);
    assert_eq!(reaped, 0, "a live owner's children must not be reaped");
    assert!(pid_alive(pid));
    assert!(
        dir.join(format!("photonic-children-{owner}.json")).exists(),
        "a live owner's registry file must survive"
    );
    child.kill().ok();
    let _ = child.wait();
    std::fs::remove_dir_all(&dir).ok();
}
