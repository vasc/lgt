use crate::store::PipelineProc;

pub fn my_pid() -> i32 {
    i32::try_from(std::process::id()).unwrap_or(0)
}

pub fn my_pgid() -> i32 {
    // SAFETY: getpgrp takes no arguments and only reads process state.
    unsafe { libc::getpgrp() }
}

pub fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 performs permission/existence checks only.
    // EPERM (exists, different owner) counts as dead: captures always run as
    // the store's owner, so a foreign-owned pid can only be a recycled one.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0
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
