//! Starting a child process, retrying the transient "text file busy".
//!
//! On Linux, `exec` fails with `ETXTBSY` while any process holds the
//! program open for writing. A program written just before it is run (a
//! freshly installed companion, or a stub in tests) can still be held by a
//! child that another thread forked meanwhile: the child inherits the
//! write descriptor until its own `exec` closes it, a matter of
//! milliseconds. Retrying with a short backoff rides that window out.

use std::io::{self, ErrorKind};
use std::process::{Child, Command};
use std::time::Duration;

/// How many times a busy program is retried, and the first pause.
const BUSY_RETRIES: u32 = 5;
const FIRST_PAUSE: Duration = Duration::from_millis(10);

/// `command.spawn()`, retried while the program is busy being written.
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    retry_busy(|| command.spawn())
}

/// Call `attempt` until it does not fail with "text file busy", at most
/// [`BUSY_RETRIES`] more times, doubling the pause each time.
pub fn retry_busy<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut pause = FIRST_PAUSE;
    for _ in 0..BUSY_RETRIES {
        match attempt() {
            Err(e) if e.kind() == ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(pause);
                pause *= 2;
            }
            result => return result,
        }
    }
    attempt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy() -> io::Error {
        io::Error::from(ErrorKind::ExecutableFileBusy)
    }

    #[test]
    fn a_busy_program_is_retried_until_it_starts() {
        let mut calls = 0;
        let result = retry_busy(|| {
            calls += 1;
            if calls < 3 { Err(busy()) } else { Ok(calls) }
        });
        assert_eq!(result.unwrap(), 3);
    }

    #[test]
    fn other_errors_and_a_program_that_stays_busy_are_returned() {
        let mut calls = 0;
        let err = retry_busy::<()>(|| {
            calls += 1;
            Err(io::Error::from(ErrorKind::NotFound))
        })
        .unwrap_err();
        assert_eq!((err.kind(), calls), (ErrorKind::NotFound, 1));
        let mut calls = 0;
        let err = retry_busy::<()>(|| {
            calls += 1;
            Err(busy())
        })
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ExecutableFileBusy);
        assert_eq!(calls, BUSY_RETRIES + 1);
    }

    /// The race itself: the program is still open for writing (as in a
    /// child forked by another thread) when it is first started.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_program_still_open_for_writing_starts_once_it_is_closed() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("stub");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let busy = Command::new(&path).spawn().unwrap_err();
        assert_eq!(busy.kind(), ErrorKind::ExecutableFileBusy);
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(file);
        });
        let status = spawn(&mut Command::new(&path)).unwrap().wait().unwrap();
        holder.join().unwrap();
        assert!(status.success());
    }
}
