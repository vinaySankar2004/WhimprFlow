//! Send the app's diagnostics to a file, because launched from `/Applications`
//! nothing is listening on stderr.
//!
//! Every `eprintln!("[whimpr] …")` in this codebase — and there are dozens, each
//! written at the moment somebody was debugging the thing it describes — goes
//! nowhere when the app is opened from Finder. So the careful explanation of *why*
//! a paste came out raw exists and is unreadable, and supporting someone else's Mac
//! means guessing.
//!
//! This captures **file descriptor 2** rather than replacing the print sites. Three
//! reasons, and the first is the one that matters:
//!
//! 1. It catches everything already written, including the panic handler's
//!    backtrace and the **cleanup worker's** own stderr, which it inherits from us.
//!    A macro would have to be threaded through two crates and a second binary, and
//!    would still miss panics.
//! 2. No call-site churn, so the diff does not bury the behavior change.
//! 3. Anything added later is captured without remembering to.
//!
//! Lines are timestamped on the way through, which is the whole point of having
//! them: "it broke" is worth little, "it broke at 11:14:23, six seconds after the
//! cloud call failed" is a diagnosis.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Rotate once the log passes this, keeping one previous file. Small on purpose: it
/// is a tail for diagnosis, not an archive, and the usage history that *is* worth
/// keeping lives in `stats.json` as structured records.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

static INSTALLED: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn pipe(fds: *mut i32) -> i32;
    fn dup(fd: i32) -> i32;
    fn dup2(old: i32, new: i32) -> i32;
    fn isatty(fd: i32) -> i32;
}

/// `~/Library/Application Support/WhimprFlow/logs`
fn log_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Library/Application Support/WhimprFlow/logs")
}

pub fn log_path() -> PathBuf {
    log_dir().join("whimpr.log")
}

/// Point stderr at the log file, timestamping each line.
///
/// Call once, as early in startup as possible — anything printed before this is
/// lost. Safe to call again; the second call does nothing.
///
/// When stderr is a terminal (`./dev.sh`, or the binary run by hand) the lines are
/// echoed there as well, so developing does not mean tailing a file to see output
/// that used to be right in front of you.
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let dir = log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return; // No log is bad; failing to start because of it is worse.
    }
    rotate_if_large(&log_path());

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path()) else {
        return;
    };

    // SAFETY: plain POSIX fd juggling. `pipe` fills two ints; `dup` snapshots the
    // real stderr before it is replaced; `dup2` retargets fd 2 so every existing
    // `eprintln!` and the worker's inherited stderr flow into the pipe instead.
    let (read_fd, write_fd, original_stderr, stderr_was_tty) = unsafe {
        let mut fds: [i32; 2] = [0; 2];
        if pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        let was_tty = isatty(2) == 1;
        let saved = dup(2);
        if dup2(fds[1], 2) < 0 {
            return;
        }
        (fds[0], fds[1], saved, was_tty)
    };
    // fd 2 now owns a duplicate of the write end, so the pipe stays open for the
    // life of the process and the reader below never sees EOF. Deliberate: an EOF
    // would end the thread, and a dead reader means writes to stderr block once the
    // pipe's buffer fills — the app would hang, silently, in whatever it logged next.
    let _ = write_fd;

    let mut echo = stderr_was_tty
        .then(|| unsafe { File::from_raw_fd(original_stderr) });

    std::thread::spawn(move || {
        let reader = BufReader::new(unsafe { File::from_raw_fd(read_fd) });
        let mut header = format!(
            "=== WhimprFlow {} started ===\n",
            env!("CARGO_PKG_VERSION")
        );
        header.insert_str(0, &stamp());
        let _ = file.write_all(header.as_bytes());
        for line in reader.lines() {
            // A read error is not a reason to stop reading: stopping is what wedges
            // the app. Skip the line and carry on.
            let Ok(line) = line else { continue };
            if let Some(out) = echo.as_mut() {
                let _ = writeln!(out, "{line}");
            }
            // Errors ignored on purpose — a full disk must not block a dictation.
            let _ = file.write_all(stamp().as_bytes());
            let _ = file.write_all(line.as_bytes());
            let _ = file.write_all(b"\n");
            let _ = file.flush();
        }
    });
}

/// Keep one previous log so a crash is still readable after a restart appends.
fn rotate_if_large(path: &Path) {
    let too_big = std::fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(false);
    if too_big {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

/// `YYYY-MM-DD HH:MM:SS ` in local time, matching what a person reads off a clock
/// when they say when something broke.
fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let t = local_time(secs);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} ",
        t.0, t.1, t.2, t.3, t.4, t.5
    )
}

/// (year, month, day, hour, minute, second) in the machine's local zone.
fn local_time(epoch_secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    // `localtime_r` rather than hand-rolled arithmetic: it applies the machine's
    // zone and its DST rules, which is the difference between a timestamp that
    // matches the user's clock and one that is an hour out for half the year.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time = epoch_secs as libc::time_t;
    unsafe { libc::localtime_r(&time, &mut tm) };
    (
        tm.tm_year + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
        tm.tm_sec as u32,
    )
}
