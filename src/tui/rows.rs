use std::collections::{BTreeMap, HashSet};

use crate::store::{Liveness, StreamMeta};

pub struct GroupRow {
    pub gid: String,
    pub label: String,
    pub collapsed: bool,
    pub running: bool,
    pub count: usize,
}

pub enum Row {
    Group(GroupRow),
    Stream(String),
}

/// Group streams by group id, newest group first, streams inside a group in
/// start order (which approximates pipeline order).
pub fn build_rows(streams: &[StreamMeta], collapsed: &HashSet<String>) -> Vec<Row> {
    let mut by_group: BTreeMap<&str, Vec<&StreamMeta>> = BTreeMap::new();
    for meta in streams {
        by_group.entry(&meta.group_id).or_default().push(meta);
    }
    let mut groups: Vec<(&str, Vec<&StreamMeta>)> = by_group.into_iter().collect();
    for (_, members) in &mut groups {
        members.sort_by_key(|m| (m.started_at_ms, m.pid));
    }
    groups.sort_by_key(|(_, members)| {
        std::cmp::Reverse(members.iter().map(|m| m.started_at_ms).max().unwrap_or(0))
    });

    let mut rows = Vec::new();
    for (gid, members) in groups {
        let is_collapsed = collapsed.contains(gid);
        rows.push(Row::Group(GroupRow {
            gid: gid.to_string(),
            label: group_label(&members),
            collapsed: is_collapsed,
            running: members.iter().any(|m| m.liveness() == Liveness::Running),
            count: members.len(),
        }));
        if !is_collapsed {
            for member in members {
                rows.push(Row::Stream(member.id.clone()));
            }
        }
    }
    rows
}

/// Human label for a group: an explicit name if one was given, otherwise the
/// pipeline reconstructed (best effort) from process-group snapshots,
/// otherwise the start time.
pub fn group_label(members: &[&StreamMeta]) -> String {
    if let Some(name) = members.iter().find_map(|m| m.group_name.clone()) {
        return name;
    }
    let mut commands: BTreeMap<i32, &str> = BTreeMap::new();
    for member in members {
        if let Some(pipeline) = &member.pipeline {
            for proc in pipeline {
                commands.entry(proc.pid).or_insert(&proc.command);
            }
        }
    }
    if !commands.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        for command in commands.values() {
            let short = short_command(command);
            // Consecutive duplicates (e.g. a script shell's helper processes
            // sharing the pgid) add noise, not information.
            if parts.last() != Some(&short) {
                parts.push(short);
            }
        }
        return parts.join(" | ");
    }
    let started = members.iter().map(|m| m.started_at_ms).min().unwrap_or(0);
    format!("pipeline · {}", crate::timefmt::clock(started))
}

fn short_command(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or(command);
    let base = first.rsplit('/').next().unwrap_or(first);
    base.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::PipelineProc;
    use crate::tui::app::tests::fake_meta;

    #[test]
    fn group_label_prefers_name_then_pipeline() {
        let mut named = fake_meta("s-1", None, "g-1", 1_000);
        named.group_name = Some("deploy".to_string());
        assert_eq!(group_label(&[&named]), "deploy");

        let mut piped = fake_meta("s-2", None, "g-2", 1_000);
        piped.pipeline = Some(vec![
            PipelineProc {
                pid: 10,
                command: "/usr/bin/make build".to_string(),
            },
            PipelineProc {
                pid: 11,
                command: "lgt build".to_string(),
            },
        ]);
        assert_eq!(group_label(&[&piped]), "make | lgt");

        let mut noisy = fake_meta("s-3", None, "g-3", 1_000);
        noisy.pipeline = Some(vec![
            PipelineProc {
                pid: 20,
                command: "/bin/zsh -c script".to_string(),
            },
            PipelineProc {
                pid: 21,
                command: "/bin/zsh helper".to_string(),
            },
            PipelineProc {
                pid: 22,
                command: "lgt build".to_string(),
            },
        ]);
        assert_eq!(
            group_label(&[&noisy]),
            "zsh | lgt",
            "consecutive duplicate commands collapse"
        );
    }
}
