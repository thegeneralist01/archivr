//! Subprocess runner with a wall-clock timeout.
//!
//! `archivr-core` deliberately has no async runtime and the tree carries no
//! `wait_timeout` dependency, so the timeout is enforced by structure: stdout
//! and stderr are drained on their own threads (a chatty child must never
//! block on a full pipe buffer), stdin is written on a third thread (a large
//! prompt can exceed the pipe buffer), and the calling thread polls
//! `try_wait` until the child exits or the deadline passes, then kills it.

use anyhow::{Context, Result, anyhow};
use std::{
    ffi::OsString,
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// Bytes of stderr kept for diagnostics.
const STDERR_TAIL_BYTES: usize = 4096;
/// Characters of the stderr tail quoted in a non-zero-exit error.
const EXIT_ERROR_STDERR_CHARS: usize = 400;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long to wait for the pipe readers after the direct child exits before
/// assuming a grandchild holds the pipes and killing the process group.
pub(crate) const READER_GRACE: Duration = Duration::from_secs(2);

/// Puts the child in its own process group (unix) so a timeout can kill the
/// whole tree, including grandchildren that inherited the output pipes.
pub(crate) fn isolate_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// SIGKILLs the process group led by `pid` (spawned via
/// [`isolate_process_group`]). Best effort; a missing group (`ESRCH`) is not
/// an error. Calls `kill(2)` directly: slim runtime images ship no `kill`
/// binary.
pub(crate) fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    {
        // kill(0, ..) hits our own group and kill(-1, ..) every process we may
        // signal; a pid that doesn't fit pid_t can't be a real child either.
        let Ok(pgid) = libc::pid_t::try_from(pid) else {
            return;
        };
        if pgid <= 1 {
            return;
        }
        // SAFETY: kill(2) takes plain integers and touches no memory of ours;
        // a negative pid targets the process group `pgid`.
        let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// Kills the child's whole process group and reaps the direct child.
pub(crate) fn kill_tree(child: &mut Child) {
    kill_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// Receives a reader result after the direct child exited: waits up to
/// `min(grace, budget)`, then kills the process group (a grandchild holding
/// the pipe) and waits one more grace period. `None` if still not done.
pub(crate) fn recv_after_exit<T>(rx: &mpsc::Receiver<T>, pid: u32, budget: Duration) -> Option<T> {
    if let Ok(v) = rx.recv_timeout(READER_GRACE.min(budget)) {
        return Some(v);
    }
    kill_process_group(pid);
    rx.recv_timeout(READER_GRACE).ok()
}

#[derive(Debug)]
pub(crate) struct ProcessOutput {
    pub stdout: String,
    /// Last 4 KiB of stderr, lossy UTF-8.
    #[allow(dead_code)]
    pub stderr_tail: String,
}

/// Sentinel at the root of a timeout error, so callers can recognise a timeout
/// without string matching (see [`is_process_timeout`]).
#[derive(Debug)]
pub(crate) struct ProcessTimedOut {
    pub secs: u64,
}

impl std::fmt::Display for ProcessTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timed out after {}s", self.secs)
    }
}

impl std::error::Error for ProcessTimedOut {}

/// True when `error` came from a [`run_with_timeout`] deadline, however many
/// context layers have been added on top since.
pub(crate) fn is_process_timeout(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|c| c.downcast_ref::<ProcessTimedOut>())
        .or_else(|| error.downcast_ref::<ProcessTimedOut>())
        .is_some()
}

/// Spawns `executable args…`, optionally writes `stdin`, drains stdout and
/// stderr on their own threads, and kills the child if it is still running at
/// `timeout`.
///
/// - Non-zero exit → `Err("{exe} exited with {status}: {last 400 chars of stderr}")`.
/// - Timeout → an error whose root is [`ProcessTimedOut`] with the message
///   `"{exe} timed out after {secs}s"`.
pub(crate) fn run_with_timeout(
    executable: &Path,
    args: &[OsString],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<ProcessOutput> {
    let exe = executable.display().to_string();
    let started = Instant::now();
    let timeout_error = || {
        let secs = timeout.as_secs().max(1);
        anyhow::Error::new(ProcessTimedOut { secs }).context(format!("{exe} timed out after {secs}s"))
    };

    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_process_group(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {exe}"))?;
    let pid = child.id();

    if let Some(input) = stdin {
        let mut pipe = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to open stdin for {exe}"))?;
        let owned = input.to_string();
        thread::spawn(move || {
            let _ = pipe.write_all(owned.as_bytes());
            // Dropping the pipe closes it, which tells the child input is complete.
        });
    }

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to open stdout for {exe}"))?;
    let (out_tx, out_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = String::new();
        let res = stdout.read_to_string(&mut buf).map(|_| buf);
        let _ = out_tx.send(res);
    });

    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to open stderr for {exe}"))?;
    let (err_tx, err_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    tail.extend_from_slice(&chunk[..n]);
                    if tail.len() > STDERR_TAIL_BYTES {
                        let excess = tail.len() - STDERR_TAIL_BYTES;
                        tail.drain(..excess);
                    }
                }
            }
        }
        let _ = err_tx.send(String::from_utf8_lossy(&tail).into_owned());
    });

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_tree(&mut child);
                return Err(anyhow::Error::new(e).context(format!("failed to wait for {exe}")));
            }
        }
        if started.elapsed() >= timeout {
            // Kill the whole group so grandchildren release the pipes too.
            kill_tree(&mut child);
            return Err(timeout_error());
        }
        thread::sleep(POLL_INTERVAL);
    };

    // The child has exited, but a grandchild that inherited the pipes can keep
    // them open; give the readers a short grace, then kill the group.
    let remaining = || timeout.saturating_sub(started.elapsed());
    let collected = match recv_after_exit(&out_rx, pid, remaining()) {
        Some(res) => res.with_context(|| format!("failed to read stdout of {exe}"))?,
        None => return Err(timeout_error()),
    };
    let Some(stderr_tail) = recv_after_exit(&err_rx, pid, remaining()) else {
        return Err(timeout_error());
    };

    if !status.success() {
        anyhow::bail!(
            "{exe} exited with {status}: {}",
            last_chars(stderr_tail.trim(), EXIT_ERROR_STDERR_CHARS)
        );
    }
    Ok(ProcessOutput {
        stdout: collected,
        stderr_tail,
    })
}

fn last_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let tail: String = s.chars().skip(count - max).collect();
    format!("…{tail}")
}

/// Shared by process-group tests here and in `downloader::ytdlp`.
#[cfg(all(test, unix))]
pub(crate) mod test_support {
    use std::{path::Path, process::Command, time::{Duration, Instant}};

    /// Shell snippet: start a background `sleep 30` and record its pid in `pid_file`.
    pub(crate) fn spawn_grandchild_snippet(pid_file: &Path) -> String {
        format!("sleep 30 & echo $! > '{}'; ", pid_file.display())
    }

    /// Reads the pid written by [`spawn_grandchild_snippet`] and asserts the
    /// process disappears within a few seconds (allowing init to reap it).
    pub(crate) fn assert_grandchild_gone(pid_file: &Path) {
        let pid = std::fs::read_to_string(pid_file).unwrap().trim().to_string();
        assert!(!pid.is_empty(), "grandchild pid not recorded");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let alive = Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !alive {
                return;
            }
            if Instant::now() >= deadline {
                let _ = Command::new("kill").args(["-KILL", &pid]).status();
                panic!("grandchild {pid} survived the timeout kill");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn run_with_timeout_drains_large_stderr_without_deadlock() {
        let out = run_with_timeout(
            Path::new("sh"),
            &os(&["-c", "head -c 1000000 /dev/zero | tr '\\0' x >&2; echo ok"]),
            None,
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(out.stdout, "ok\n");
        assert_eq!(out.stderr_tail.len(), STDERR_TAIL_BYTES);
    }

    #[test]
    fn run_with_timeout_kills_overrunning_child_and_marks_timeout() {
        let started = Instant::now();
        let err = run_with_timeout(
            Path::new("sleep"),
            &os(&["30"]),
            None,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(is_process_timeout(&err), "{err:#}");
        assert!(format!("{err:#}").contains("timed out after 1s"), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
        // Still recognisable under further context layers.
        let wrapped = err.context("outer").context("outermost");
        assert!(is_process_timeout(&wrapped));
    }

    #[test]
    fn run_with_timeout_reports_nonzero_exit_with_stderr_tail() {
        let err = run_with_timeout(
            Path::new("sh"),
            &os(&["-c", "echo first-line >&2; echo boom-at-the-end >&2; exit 3"]),
            None,
            Duration::from_secs(30),
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("exited with"), "{msg}");
        assert!(msg.contains("boom-at-the-end"), "{msg}");
        assert!(!is_process_timeout(&err));
    }

    #[test]
    fn run_with_timeout_round_trips_stdin() {
        let out = run_with_timeout(
            Path::new("cat"),
            &[],
            Some("prompt text"),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(out.stdout, "prompt text");
    }

    #[test]
    fn last_chars_keeps_the_tail() {
        assert_eq!(last_chars("abc", 5), "abc");
        assert_eq!(last_chars("abcdef", 3), "…def");
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_grandchildren_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let script = format!("{}sleep 30", test_support::spawn_grandchild_snippet(&pid_file));
        let started = Instant::now();
        let err = run_with_timeout(Path::new("sh"), &os(&["-c", &script]), None, Duration::from_secs(1))
            .unwrap_err();
        assert!(is_process_timeout(&err), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
        test_support::assert_grandchild_gone(&pid_file);
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_does_not_wait_out_budget_for_pipe_holding_grandchild() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let script = format!("{}echo done", test_support::spawn_grandchild_snippet(&pid_file));
        let started = Instant::now();
        let out = run_with_timeout(Path::new("sh"), &os(&["-c", &script]), None, Duration::from_secs(60))
            .unwrap();
        assert_eq!(out.stdout, "done\n");
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        test_support::assert_grandchild_gone(&pid_file);
    }
}
