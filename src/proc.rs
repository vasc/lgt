use crate::store::PipelineProc;

pub fn my_pid() -> i32 {
    i32::try_from(std::process::id()).unwrap_or(0)
}

pub fn my_pgid() -> i32 {
    // SAFETY: getpgrp takes no arguments and only reads process state.
    unsafe { libc::getpgrp() }
}

/// Whether a process with this pid exists. EPERM (it exists but belongs to
/// another user) counts as alive: a "died" verdict is persisted and makes a
/// stream eligible for archiving and deletion, so when unsure we must err
/// towards "still running".
pub fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 sends nothing; it only performs the
    // existence/permission check on a plain integer pid.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// A stable identifier of this machine, read once. Captures record it so
/// that a store shared between machines (NFS home, container mounts) never
/// probes a pid that belongs to another machine. Unlike the hostname, it
/// does not change with the network the machine is on.
pub fn machine_id() -> Option<&'static str> {
    static MACHINE_ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    MACHINE_ID.get_or_init(read_machine_id).as_deref()
}

/// This process's PID namespace (Linux), e.g. `pid:[4026531836]`. Two
/// containers on one machine — even sharing a machine id, or having none —
/// see different pids for the same process; a pid recorded in another
/// namespace means nothing here. `None` where there are no namespaces.
pub fn pid_namespace() -> Option<&'static str> {
    static PID_NS: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    PID_NS
        .get_or_init(|| {
            std::fs::read_link("/proc/self/ns/pid")
                .ok()
                .map(|target| target.to_string_lossy().into_owned())
        })
        .as_deref()
}

/// The hardware UUID (the one `ioreg` reports as IOPlatformUUID).
#[cfg(target_os = "macos")]
fn read_machine_id() -> Option<String> {
    let mut uuid = [0u8; 16];
    let wait = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    // SAFETY: `uuid` is a live, writable 16-byte buffer — exactly the
    // `uuid_t` gethostuuid fills — and `wait` is a valid timespec that
    // outlives the call.
    let rc = unsafe { libc::gethostuuid(uuid.as_mut_ptr(), &wait) };
    (rc == 0).then(|| uuid.iter().map(|b| format!("{b:02x}")).collect())
}

/// systemd's machine id (or D-Bus's, on systems without systemd).
#[cfg(not(target_os = "macos"))]
fn read_machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|id| id.trim().to_ascii_lowercase())
        .filter(|id| !id.is_empty())
}

/// Merge two pipeline snapshots keyed by pid. Entries only present in one
/// side are kept; on conflict the later snapshot's command wins (it is the
/// post-exec view).
pub fn merge_snapshots(earlier: Vec<PipelineProc>, later: Vec<PipelineProc>) -> Vec<PipelineProc> {
    let mut by_pid: std::collections::BTreeMap<i32, String> =
        earlier.into_iter().map(|p| (p.pid, p.command)).collect();
    for p in later {
        by_pid.insert(p.pid, p.command);
    }
    by_pid
        .into_iter()
        .map(|(pid, command)| PipelineProc { pid, command })
        .collect()
}

/// Snapshot every process in the given process group via `ps`. Used to label a
/// pipeline in the TUI (`cmd1 | lgt | cmd2 | lgt`). Best effort: returns an
/// empty list when `ps` fails.
pub fn pipeline_snapshot(pgid: i32) -> Vec<PipelineProc> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axo", "pid=,pgid=,command="])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut procs = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid_s), Some(pgid_s)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(pid), Ok(line_pgid)) = (pid_s.parse::<i32>(), pgid_s.parse::<i32>()) else {
            continue;
        };
        if line_pgid != pgid {
            continue;
        }
        let command = parts.collect::<Vec<_>>().join(" ");
        // The ps we just spawned inherits our pgid; it is not part of the pipeline.
        if command.is_empty() || command.starts_with("ps -axo pid=,pgid=,command=") {
            continue;
        }
        procs.push(PipelineProc { pid, command });
    }
    procs.sort_by_key(|p| p.pid);
    procs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_alive() {
        assert!(alive(my_pid()));
    }

    #[test]
    fn processes_owned_by_someone_else_count_as_alive() {
        // pid 1 (init/launchd) always exists and belongs to root: as a normal
        // user the probe fails with EPERM, which must not read as "dead".
        assert!(alive(1));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn machine_id_is_the_hardware_uuid() {
        let ours = machine_id().expect("machine id");
        let out = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text
            .lines()
            .find(|l| l.contains("IOPlatformUUID"))
            .expect("ioreg reports IOPlatformUUID");
        let theirs: String = line
            .rsplit('"')
            .nth(1)
            .unwrap()
            .chars()
            .filter(|c| *c != '-')
            .collect::<String>()
            .to_ascii_lowercase();
        assert_eq!(ours, theirs);
        assert_eq!(machine_id(), Some(ours), "cached value is stable");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn machine_id_is_the_systemd_machine_id_when_there_is_one() {
        // Minimal container images often have neither file.
        let theirs = std::fs::read_to_string("/etc/machine-id")
            .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
            .ok()
            .map(|id| id.trim().to_ascii_lowercase())
            .filter(|id| !id.is_empty());
        assert_eq!(machine_id(), theirs.as_deref());
    }

    #[test]
    fn pid_namespace_is_a_namespace_inode_on_linux_and_absent_elsewhere() {
        let ns = pid_namespace();
        if cfg!(target_os = "linux") {
            let ns = ns.expect("Linux exposes /proc/self/ns/pid");
            let inode = ns
                .strip_prefix("pid:[")
                .and_then(|rest| rest.strip_suffix(']'))
                .unwrap_or_else(|| panic!("unexpected namespace format: {ns}"));
            assert!(
                !inode.is_empty() && inode.bytes().all(|b| b.is_ascii_digit()),
                "{ns}"
            );
        } else {
            assert_eq!(ns, None, "no PID namespaces outside Linux");
        }
    }

    #[test]
    fn nonsense_pids_are_not_alive() {
        assert!(!alive(0));
        assert!(!alive(-4));
    }

    #[test]
    fn snapshot_contains_own_process_group() {
        let procs = pipeline_snapshot(my_pgid());
        assert!(procs.iter().any(|p| p.pid == my_pid()));
    }

    #[test]
    fn merge_keeps_departed_stages_and_prefers_later_commands() {
        let earlier = vec![
            PipelineProc {
                pid: 10,
                command: "seq 1 5".to_string(),
            },
            PipelineProc {
                pid: 11,
                command: "zsh".to_string(),
            },
        ];
        let later = vec![
            PipelineProc {
                pid: 11,
                command: "lgt step1".to_string(),
            },
            PipelineProc {
                pid: 12,
                command: "tr a-z A-Z".to_string(),
            },
        ];
        let merged = merge_snapshots(earlier, later);
        let commands: Vec<&str> = merged.iter().map(|p| p.command.as_str()).collect();
        assert_eq!(commands, vec!["seq 1 5", "lgt step1", "tr a-z A-Z"]);
    }
}
