//! What a stream is, as recorded in its `meta.json`.

use serde::{Deserialize, Serialize};

/// Status as recorded on disk. `Died` is recorded once a reader has seen
/// that a `Running` stream's capture process is gone (see
/// [`crate::store::Store::record_death`]); after that the stream can never change again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Done,
    Died,
}

/// Status as observed from outside: a stream whose meta still says `Running`
/// but whose pid is gone was killed before it could finalize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Running,
    Done,
    Died,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineProc {
    pub pid: i32,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMeta {
    pub id: String,
    pub name: Option<String>,
    pub group_id: String,
    pub group_name: Option<String>,
    pub pid: i32,
    pub pgid: i32,
    /// Stable id of the machine the capture ran on (see
    /// `proc::machine_id`); its pid means nothing anywhere else.
    #[serde(default)]
    pub machine: Option<String>,
    /// PID namespace the capture ran in (Linux; see
    /// `proc::pid_namespace`): tells containers on one machine apart.
    #[serde(default)]
    pub pid_ns: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub status: Status,
    pub bytes: u64,
    pub lines: u64,
    pub cwd: Option<String>,
    /// Path of the `-o/--out` local copy, when one was requested.
    #[serde(default)]
    pub copy: Option<String>,
    pub pipeline: Option<Vec<PipelineProc>>,
}

impl StreamMeta {
    /// True when the capture ran on another machine sharing this store. Its
    /// pid cannot be probed from here, so it is never declared dead here.
    pub fn is_foreign(&self) -> bool {
        let differs = |theirs: &Option<String>, ours: Option<&str>| match (theirs, ours) {
            (Some(theirs), Some(ours)) => theirs != ours,
            _ => false,
        };
        differs(&self.machine, crate::proc::machine_id())
            || differs(&self.pid_ns, crate::proc::pid_namespace())
    }

    pub fn liveness(&self) -> Liveness {
        match self.status {
            Status::Done => Liveness::Done,
            Status::Died => Liveness::Died,
            Status::Running => {
                if self.is_foreign() || crate::proc::alive(self.pid) {
                    Liveness::Running
                } else {
                    Liveness::Died
                }
            }
        }
    }

    /// When a finished stream last wrote (its end); `None` while running.
    pub fn finished_at_ms(&self) -> Option<i64> {
        match self.status {
            Status::Running => None,
            Status::Done | Status::Died => Some(self.ended_at_ms.unwrap_or(self.started_at_ms)),
        }
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}
