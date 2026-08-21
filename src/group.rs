use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store::{Store, validate_name};

/// How long after its creation a pipeline-group claim stays joinable. All
/// members of a shell pipeline are spawned together, so real siblings claim
/// within milliseconds of each other; after this window a claim for the same
/// pgid must belong to an older, unrelated pipeline (or a script shell whose
/// pgid every pipeline shares) and is discarded.
pub const CLAIM_WINDOW_MS: i64 = 60_000;

pub struct GroupInfo {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Claim {
    group_id: String,
    created_at_ms: i64,
}

/// Decide which group a new capture belongs to.
///
/// Priority: explicit `-g NAME` flag, then the `LGT_GROUP` environment
/// variable (both produce stable named groups usable across separate
/// pipelines), then automatic linking via the process group id every member
/// of a single shell pipeline shares.
pub fn resolve(store: &Store, pid: i32, pgid: i32, manual: Option<String>) -> Result<GroupInfo> {
    let manual = manual.or_else(|| {
        std::env::var("LGT_GROUP")
            .ok()
            .filter(|value| !value.is_empty())
    });
    if let Some(name) = manual {
        validate_name(&name)?;
        return Ok(GroupInfo {
            id: format!("named-{name}"),
            name: Some(name),
        });
    }
    let id = claim_pipeline_group(store, pid, pgid, crate::timefmt::now_ms())?;
    Ok(GroupInfo { id, name: None })
}

fn claim_path(store: &Store, pgid: i32) -> PathBuf {
    store.groups_dir().join(format!("pgid-{pgid}.json"))
}

fn read_claim(path: &PathBuf) -> Option<Claim> {
    let body = fs::read(path).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Join (or create) the group for our process group. The first `lgt` in a
/// pipeline publishes a claim file via hard-link (atomic, fails if present);
/// its siblings read it and join the same group.
pub fn claim_pipeline_group(store: &Store, pid: i32, pgid: i32, now_ms: i64) -> Result<String> {
    let path = claim_path(store, pgid);
    for _ in 0..5 {
        match read_claim(&path) {
            Some(claim) if now_ms.saturating_sub(claim.created_at_ms) <= CLAIM_WINDOW_MS => {
                return Ok(claim.group_id);
            }
            Some(_) => {
                let _ = fs::remove_file(&path);
            }
            None => {}
        }
        let group_id = format!("g-{}-{}", crate::timefmt::compact(now_ms), pid);
        let claim = Claim {
            group_id: group_id.clone(),
            created_at_ms: now_ms,
        };
        let tmp = store.groups_dir().join(format!(".claim-{pid}.tmp"));
        if fs::write(&tmp, serde_json::to_vec(&claim)?).is_err() {
            break;
        }
        let linked = fs::hard_link(&tmp, &path);
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => return Ok(group_id),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            // Any other failure (claim dir swept concurrently, exotic fs):
            // grouping is a convenience, the data matters more.
            Err(_) => break,
        }
    }
    // Contention or errors beyond retries: fall back to a solo group rather
    // than fail the capture.
    Ok(format!("g-{}-{}", crate::timefmt::compact(now_ms), pid))
}

/// Remove expired `pgid-*.json` claim files. Anything else in the directory
/// (notably another process's in-flight `.claim-*.tmp`) is left alone.
pub fn prune_stale_claims(store: &Store, now_ms: i64) {
    let Ok(entries) = fs::read_dir(store.groups_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("pgid-") || !name.ends_with(".json") {
            continue;
        }
        let path = entry.path();
        let stale = match read_claim(&path) {
            Some(claim) => now_ms.saturating_sub(claim.created_at_ms) > CLAIM_WINDOW_MS,
            None => true,
        };
        if stale {
            let _ = fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testutil::temp_store;

    #[test]
    fn siblings_join_the_same_group() {
        let store = temp_store();
        let a = claim_pipeline_group(&store, 100, 42, 1_000).unwrap();
        let b = claim_pipeline_group(&store, 101, 42, 1_500).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn stale_claims_start_a_new_group() {
        let store = temp_store();
        let a = claim_pipeline_group(&store, 100, 42, 1_000).unwrap();
        let b = claim_pipeline_group(&store, 200, 42, 1_000 + CLAIM_WINDOW_MS + 1).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn different_pgids_get_different_groups() {
        let store = temp_store();
        let a = claim_pipeline_group(&store, 100, 42, 1_000).unwrap();
        let b = claim_pipeline_group(&store, 101, 43, 1_000).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn prune_leaves_foreign_tmp_files_alone() {
        let store = temp_store();
        let tmp = store.groups_dir().join(".claim-99999.tmp");
        std::fs::write(&tmp, b"in-flight").unwrap();
        let unrelated = store.groups_dir().join("notes.txt");
        std::fs::write(&unrelated, b"keep").unwrap();
        prune_stale_claims(&store, i64::MAX);
        assert!(tmp.exists(), "another capture's tmp claim must survive");
        assert!(unrelated.exists(), "non-claim files must survive");
    }

    #[test]
    fn prune_removes_only_stale_claims() {
        let store = temp_store();
        claim_pipeline_group(&store, 100, 42, 1_000).unwrap();
        claim_pipeline_group(&store, 100, 43, 500_000).unwrap();
        prune_stale_claims(&store, 500_100);
        // pgid 42's claim is stale and gone: a rejoin creates a fresh group.
        let rejoined = claim_pipeline_group(&store, 101, 43, 500_200).unwrap();
        assert!(rejoined.contains("-100"));
        let fresh = claim_pipeline_group(&store, 102, 42, 500_200).unwrap();
        assert!(fresh.contains("-102"));
    }
}
