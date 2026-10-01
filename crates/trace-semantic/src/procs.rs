//! Process trees of the programs trace starts (language servers, the TypeScript worker, build
//! steps, ecosystem installs): nothing trace started keeps running after trace is done with it.
//!
//! * [`command`] builds a [`Command`] whose standard streams are all `null` (callers replace
//!   them with pipes or log files): a child never gets trace's own stdin / stdout / stderr, so
//!   `trace ... | tail` is never held open by a server's helper. [`isolate`] (applied by
//!   [`command`]; call it on commands built elsewhere) makes the whole tree stoppable: on Unix
//!   the child leads its own process group (`CommandExt::process_group(0)`).
//! * [`stop_tree`] stops a child and every process it started that is still running:
//!   - Unix: `kill -TERM -- -<pgid>` (the child's group, i.e. the child and its descendants
//!     that stayed in the group), up to the grace for the child to exit, a short pause for the
//!     rest, then `kill -KILL -- -<pgid>` when the group existed. The signals go through the
//!     system `kill` program (`/bin/kill`, `/usr/bin/kill`); no libc, no shell.
//!   - Windows: `%SystemRoot%\System32\taskkill.exe /T /F /PID <pid>` while the child still
//!     runs (`/T` follows parent links from the child, so it reaches every descendant only
//!     while the child is alive; trace holds the child's handle, so its PID is never reused
//!     before this). No shell, no PowerShell, no hidden-window launch.
//!   - Then the child itself is killed (if still running) and reaped.
//!
//!   Only the child's own tree is ever signalled: a process group id equals the pid of the
//!   child trace started (and exists only while that child or one of its descendants lives),
//!   and taskkill walks down from the child's pid. A descendant that deliberately leaves the
//!   tree (a daemon calling `setsid` on Unix, or whose parent already exited on Windows) is
//!   out of reach here; the build tools that start such daemons are run without them
//!   (`--no-daemon`-style options of the language setups).
//! * [`wait_bounded`] waits for a child with a timeout (stopping the tree when it passes).

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Grace for a process tree to end after the polite signal (Unix) before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(3);
/// Pause after the polite group signal for descendants of an already exited child (Unix).
#[cfg_attr(not(unix), allow(dead_code))]
const DESCENDANT_PAUSE: Duration = Duration::from_millis(200);
/// Bound of one `kill` / `taskkill` run.
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll step of the bounded waits.
const POLL: Duration = Duration::from_millis(20);

/// A command for `program` that inherits nothing of trace's standard streams (all `null`
/// until the caller sets pipes / files) and whose process tree [`stop_tree`] can stop.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    isolate(&mut cmd);
    cmd
}

/// Make the tree of the process `cmd` starts stoppable by [`stop_tree`] (Unix: a process
/// group of its own; Windows: nothing to set, taskkill follows the parent links).
pub fn isolate(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// Whether the child is still running (a child that cannot be queried counts as ended).
fn running(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(None))
}

/// Wait up to `limit` for the child to exit; `true` when it did.
#[cfg_attr(not(unix), allow(dead_code))]
fn wait_exit(child: &mut Child, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < until => std::thread::sleep(POLL),
            Ok(None) => return false,
            Err(_) => return true,
        }
    }
}

/// Stop `child` and the processes it started (module docs), then reap it. `grace` bounds
/// the wait for a polite exit (Unix `TERM`); Windows stops the tree at once.
pub fn stop_tree(child: &mut Child, grace: Duration) {
    let pid = child.id();
    #[cfg(unix)]
    {
        let group_existed = signal_group(pid, "TERM");
        if running(child) {
            let _ = wait_exit(child, grace);
        } else if group_existed {
            std::thread::sleep(DESCENDANT_PAUSE.min(grace));
        }
        if group_existed {
            let _ = signal_group(pid, "KILL");
        }
    }
    #[cfg(windows)]
    {
        let _ = grace;
        if running(child) {
            let _ = taskkill_tree(pid);
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, grace);
    }
    if running(child) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

/// Wait for `child` up to `timeout`; when it passes, its tree is stopped and `Ok(None)` is
/// returned. After a normal exit, processes it left behind in its tree are stopped too
/// (Unix process group).
pub fn wait_bounded(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            stop_leftovers(child);
            return Ok(Some(status));
        }
        if started.elapsed() >= timeout {
            stop_tree(child, Duration::ZERO);
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

/// After the child exited: stop what it left running in its tree (Unix: its process group,
/// which lives on while any member does; Windows: the parent links end with the child, so
/// nothing is reachable).
pub fn stop_leftovers(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        if signal_group(pid, "TERM") {
            std::thread::sleep(DESCENDANT_PAUSE);
            let _ = signal_group(pid, "KILL");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child;
    }
}

/// Run a system program (never a shell) with no standard streams, bounded; `true` when it
/// exited successfully.
fn run_system(program: &Path, args: &[String]) -> bool {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let until = Instant::now() + SIGNAL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < until => std::thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The system `kill` program (Unix).
#[cfg(unix)]
fn kill_program() -> Option<PathBuf> {
    ["/bin/kill", "/usr/bin/kill"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Send `signal` to the process group `pgid` (the group of a child trace started); `true`
/// when at least one member received it.
#[cfg(unix)]
fn signal_group(pgid: u32, signal: &str) -> bool {
    if pgid <= 1 {
        return false;
    }
    let Some(kill) = kill_program() else {
        return false;
    };
    run_system(&kill, &group_signal_args(pgid, signal))
}

/// `kill` arguments signalling the whole process group `pgid`.
#[cfg_attr(not(unix), allow(dead_code))]
fn group_signal_args(pgid: u32, signal: &str) -> Vec<String> {
    vec![format!("-{signal}"), "--".to_string(), format!("-{pgid}")]
}

/// `taskkill.exe` of this Windows installation (`%SystemRoot%\System32`).
#[cfg(windows)]
fn taskkill_program() -> Option<PathBuf> {
    let root = trace_core::env::system_root().map(PathBuf::from)?;
    let exe = root.join("System32").join("taskkill.exe");
    exe.is_file().then_some(exe)
}

/// Force-stop the process `pid` and its descendants (Windows).
#[cfg(windows)]
fn taskkill_tree(pid: u32) -> bool {
    let Some(taskkill) = taskkill_program() else {
        return false;
    };
    run_system(&taskkill, &tree_kill_args(pid))
}

/// `taskkill` arguments stopping the tree of `pid`.
#[cfg_attr(not(windows), allow(dead_code))]
fn tree_kill_args(pid: u32) -> Vec<String> {
    vec!["/T".to_string(), "/F".to_string(), "/PID".to_string(), pid.to_string()]
}

#[cfg(test)]
#[path = "../tests/unit/procs.rs"]
mod tests;
