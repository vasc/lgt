use std::collections::{BTreeMap, HashMap, HashSet};

use crate::store::Liveness;
use crate::tui::catalog::{Catalog, Entry};

pub struct GroupRow {
    pub gid: String,
    pub label: String,
    pub collapsed: bool,
    pub running: bool,
    /// Member stream ids in start order.
    pub members: Vec<String>,
}

pub enum Row {
    Group(GroupRow),
    Stream(String),
}

/// Group streams by group id, newest group first, streams inside a group in
/// start order (which approximates pipeline order).
pub fn build_rows(catalog: &Catalog, collapsed: &HashSet<String>) -> Vec<Row> {
    let mut by_group: HashMap<&str, Vec<&Entry>> = HashMap::new();
    for entry in catalog.entries() {
        by_group
            .entry(&entry.meta.group_id)
            .or_default()
            .push(entry);
    }
    let mut groups: Vec<(i64, &str, Vec<&Entry>)> = by_group
        .into_iter()
        .map(|(gid, mut members)| {
            members.sort_by(|a, b| {
                (a.meta.started_at_ms, a.meta.pid, &a.meta.id).cmp(&(
                    b.meta.started_at_ms,
                    b.meta.pid,
                    &b.meta.id,
                ))
            });
            let newest = members.last().map_or(0, |e| e.meta.started_at_ms);
            (newest, gid, members)
        })
        .collect();
    groups.sort_by(|(na, ga, _), (nb, gb, _)| nb.cmp(na).then_with(|| ga.cmp(gb)));

    let mut rows = Vec::with_capacity(catalog.len() + groups.len());
    for (_, gid, members) in groups {
        let is_collapsed = collapsed.contains(gid);
        rows.push(Row::Group(GroupRow {
            gid: gid.to_string(),
            label: group_label(&members),
            collapsed: is_collapsed,
            running: members.iter().any(|e| e.liveness == Liveness::Running),
            members: members.iter().map(|e| e.meta.id.clone()).collect(),
        }));
        if !is_collapsed {
            for member in members {
                rows.push(Row::Stream(member.meta.id.clone()));
            }
        }
    }
    rows
}

/// Human label for a group: an explicit name if one was given, otherwise the
/// pipeline reconstructed (best effort) from process-group snapshots,
/// otherwise the start time.
pub fn group_label(members: &[&Entry]) -> String {
    if let Some(name) = members.iter().find_map(|e| e.meta.group_name.clone()) {
        return name;
    }
    let mut commands: BTreeMap<i32, &str> = BTreeMap::new();
    for member in members {
        if let Some(pipeline) = &member.meta.pipeline {
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
    let started = members
        .iter()
        .map(|e| e.meta.started_at_ms)
        .min()
        .unwrap_or(0);
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
    use crate::store::testutil::meta;

    fn label_of(metas: Vec<crate::store::StreamMeta>) -> String {
        let catalog = Catalog::from_metas(metas);
        let entries: Vec<&Entry> = catalog.entries().collect();
        group_label(&entries)
    }

    #[test]
    fn group_label_prefers_name_then_pipeline() {
        let mut named = meta("s-1", None, 1_000);
        named.group_name = Some("deploy".to_string());
        assert_eq!(label_of(vec![named]), "deploy");

        let mut piped = meta("s-2", None, 1_000);
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
        assert_eq!(label_of(vec![piped]), "make | lgt");

        let mut noisy = meta("s-3", None, 1_000);
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
            label_of(vec![noisy]),
            "zsh | lgt",
            "consecutive duplicate commands collapse"
        );
    }
}
