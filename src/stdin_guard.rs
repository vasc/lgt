//! Subcommands never read stdin. A *bare* subcommand with live piped input
//! (`cmd | lgt ls`, `echo out | lgt archive`) is almost certainly a capture
//! whose name collides with a subcommand; running it would silently swallow
//! the pipeline's output, so it is refused. A subcommand given arguments
//! (`lgt rm "$id"` inside a `while read` loop) is deliberate and runs. An
//! empty pipe whose writer is gone (how cron and similar runners start
//! jobs) carries no output and is allowed either way.

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::FileTypeExt;

/// How long to wait for a pipe's writer to close (or write) before treating
/// it as live: cron closes its end just after starting the job.
const GRACE_MS: i32 = 100;

/// True when `fd` is a pipe or socket that either has data waiting or still
/// has a writer attached (more may come).
pub fn is_live_pipe(fd: BorrowedFd) -> bool {
    let Ok(owned) = fd.try_clone_to_owned() else {
        return false;
    };
    let mut file = File::from(owned);
    let Ok(kind) = file.metadata().map(|m| m.file_type()) else {
        return false;
    };
    if !(kind.is_fifo() || kind.is_socket()) {
        return false;
    }
    let mut poll = libc::pollfd {
        fd: file.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll` points to one valid, live pollfd for a descriptor we
    // own; nfds is 1 and the timeout bounds the wait to GRACE_MS.
    let ready = unsafe { libc::poll(&mut poll, 1, GRACE_MS) };
    if ready <= 0 {
        // Nothing to read yet (a writer is still attached), or poll failed:
        // either way we cannot rule out a live pipeline.
        return true;
    }
    // Readable means data is waiting or the writer is gone; one byte tells
    // which. (When it is data, we are refusing anyway.)
    let mut byte = [0u8; 1];
    !matches!(file.read(&mut byte), Ok(0))
}

/// Refuse to run a bare `lgt <command>` with live piped input.
pub fn refuse_piped_stdin(command: &str, bare: bool) -> anyhow::Result<()> {
    if bare && is_live_pipe(std::io::stdin().as_fd()) {
        anyhow::bail!(
            "`lgt {command}` does not read its input, but something is piped into it; \
             to capture, pipe into `lgt NAME` instead. If the pipe is intentional (e.g. \
             over ssh), redirect stdin: `lgt {command} … </dev/null`"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_pipe_with_a_live_writer_is_live() {
        let (reader, _writer) = std::io::pipe().unwrap();
        assert!(is_live_pipe(reader.as_fd()));
    }

    #[test]
    fn a_pipe_with_data_is_live() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"output").unwrap();
        drop(writer);
        assert!(is_live_pipe(reader.as_fd()));
    }

    #[test]
    fn an_empty_closed_pipe_is_not_live() {
        // What cron hands a job without `%` input.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(writer);
        assert!(!is_live_pipe(reader.as_fd()));
    }

    #[test]
    fn regular_files_and_devices_are_not_pipes() {
        let null = File::open("/dev/null").unwrap();
        assert!(!is_live_pipe(null.as_fd()));
        let path = std::env::temp_dir().join(format!("lgt-guard-{}", std::process::id()));
        std::fs::write(&path, b"data").unwrap();
        let file = File::open(&path).unwrap();
        assert!(!is_live_pipe(file.as_fd()));
    }

    #[test]
    fn sockets_are_checked_too() {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(is_live_pipe(ours.as_fd()), "peer still connected");
        drop(theirs);
        assert!(!is_live_pipe(ours.as_fd()), "peer gone, nothing sent");
    }
}
