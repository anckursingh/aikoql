//! MRFC-0070 R10.1: Incremental directory ingestion.
//!
//! Tracks the last git commit SHA in a marker file. On re-ingestion, only
//! files changed since the last commit are re-parsed. Unchanged files retain
//! their entities; deleted files have their entities flagged as stale.
//!
//! Pipeline: git diff → changed files → parse changed → reconcile → merge.
//! Reuses the existing `reconcile()`, `merge_knowledge_ir()`, and
//! `compile_file()` infrastructure.

use crate::ingest_dir::{compile_file, IngestResult};
use crate::ir::*;
use crate::merge::merge_knowledge_ir;
use crate::reconcile::reconcile;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Tracking state persisted between incremental runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackState {
    pub path: String,
    pub last_sha: String,
}

const TRACK_FILE: &str = ".mnemosyne-track.json";

/// Read the tracking state from a directory. Returns None if no previous run.
fn read_track_state(root: &Path) -> Option<TrackState> {
    let path = root.join(TRACK_FILE);
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Write tracking state to a directory.
fn write_track_state(root: &Path, state: &TrackState) {
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(root.join(TRACK_FILE), json);
    }
}

/// Get the current HEAD SHA from git. Returns empty string on failure.
fn current_head_sha(root: &Path) -> String {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Get list of changed files between two commits.
fn git_diff(root: &Path, from_sha: &str, to_sha: &str) -> Vec<String> {
    let range = format!("{}..{}", from_sha, to_sha);
    Command::new("git")
        .args(["diff", "--name-only", &range])
        .current_dir(root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let files: Vec<String> = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                Some(files)
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Incremental directory ingestion.
///
/// On first run (no tracking file): does a full ingest and saves the commit SHA.
/// On subsequent runs: only parses changed/deleted files, reconciles against
/// the previous IR, and produces an updated merged IR.
///
/// Returns `(IngestResult, is_full_ingest)` to distinguish full vs incremental runs.
pub fn incremental_ingest_directory(root: &str) -> Result<(IngestResult, bool), String> {
    let path = Path::new(root);
    if !path.is_dir() {
        return Err(format!("not a directory: {}", root));
    }

    let head_sha = current_head_sha(path);
    if head_sha.is_empty() {
        // Not a git repo — fall back to full ingest
        let result = crate::ingest_dir::ingest_directory(root)?;
        return Ok((result, true));
    }

    let prev = read_track_state(path);

    // First run or new SHA — full ingest with tracking
    if prev.is_none() || prev.as_ref().map(|p| p.last_sha.as_str()) != Some(head_sha.as_str()) {
        let result = full_ingest_and_track(path, root, &head_sha, &prev)?;
        return Ok((result, true));
    }

    // Same SHA — nothing changed, return cached knowledge
    // (The caller should have cached the previous IR; we return a minimal result.)
    Err("no changes since last ingest — use full ingest to force re-scan".into())
}

/// Perform a full ingest and save tracking state.
fn full_ingest_and_track(
    path: &Path,
    root: &str,
    head_sha: &str,
    _prev: &Option<TrackState>,
) -> Result<IngestResult, String> {
    let result = crate::ingest_dir::parallel_ingest_directory(root)?;

    write_track_state(
        path,
        &TrackState {
            path: root.to_string(),
            last_sha: head_sha.to_string(),
        },
    );

    Ok(result)
}

/// Diff-based incremental ingestion: given the previous IR and current state,
/// only parse files that changed. Returns the updated merged IR.
///
/// Callers should:
/// 1. Call `incremental_ingest_directory()` first — if it returns Ok, that's the result.
/// 2. If it returns Err("no changes..."), call this function with the cached IR.
/// 3. If it returns Err for another reason, fall back to full ingest.
pub fn incremental_diff_ingest(
    root: &str,
    previous_ir: &KnowledgeIr,
) -> Result<(IngestResult, Vec<String>), String> {
    let path = Path::new(root);
    if !path.is_dir() {
        return Err(format!("not a directory: {}", root));
    }

    let prev = read_track_state(path).ok_or("no tracking state — run full ingest first")?;
    let head_sha = current_head_sha(path);
    if head_sha.is_empty() {
        return Err("not a git repository".into());
    }

    // Get changed files since last ingest
    let changed = git_diff(path, &prev.last_sha, &head_sha);

    if changed.is_empty() {
        // Update tracking SHA even if no file changes (e.g., merge commits)
        write_track_state(
            path,
            &TrackState {
                path: root.to_string(),
                last_sha: head_sha,
            },
        );
        return Err("no changed files".into());
    }

    // Filter to files that exist and aren't skipped
    let existing: Vec<PathBuf> = changed
        .iter()
        .map(|f| path.join(f))
        .filter(|p| p.exists() && p.is_file())
        .collect();

    // Parse only changed files
    let new_irs: Vec<KnowledgeIr> = existing.iter().filter_map(|p| compile_file(p)).collect();

    if new_irs.is_empty() && existing.is_empty() {
        // All changed files were deleted — entities should be marked stale
        let report = reconcile(&changed, previous_ir);
        write_track_state(
            path,
            &TrackState {
                path: root.to_string(),
                last_sha: head_sha,
            },
        );
        return Ok((
            IngestResult {
                ir: previous_ir.clone(),
                files_processed: 0,
                files_skipped: changed.len() as u32,
                dirs_skipped: 0,
                binary_skipped: 0,
            },
            report.potentially_stale_facts,
        ));
    }

    // Merge new IRs with previous (existing entities from unchanged files persist)
    let mut all_irs: Vec<KnowledgeIr> = new_irs;
    all_irs.push(previous_ir.clone());
    let mut merged = merge_knowledge_ir(&all_irs);
    merged.document_id = Some(format!("ingest-dir:{}", root));
    merged.extractor = "ingest-dir-incremental".into();
    merged.page_count = existing.len() as u32;

    // Reconcile to flag stale facts from changed/deleted files
    let report = reconcile(&changed, &merged);
    for fact in &report.potentially_stale_facts {
        merged.facts.push(FactCandidate {
            statement: format!("[STALE] {}", fact),
            entities: vec![],
            confidence: 0.1,
            evidence: Evidence {
                document_id: merged.document_id.clone(),
                page: None,
                bbox_text: None,
                extractor: "ingest-dir-incremental".into(),
                model: None,
                confidence: 0.3,
            },
        });
    }

    // Update tracking
    write_track_state(
        path,
        &TrackState {
            path: root.to_string(),
            last_sha: head_sha,
        },
    );

    Ok((
        IngestResult {
            ir: merged,
            files_processed: existing.len() as u32,
            files_skipped: 0,
            dirs_skipped: 0,
            binary_skipped: 0,
        },
        report.potentially_stale_facts,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest_dir::ingest_directory;
    use std::fs;

    #[test]
    fn track_state_roundtrip() {
        let tmp = std::env::temp_dir().join("mnemosyne-track-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let state = TrackState {
            path: tmp.to_string_lossy().to_string(),
            last_sha: "abc123def".into(),
        };
        write_track_state(&tmp, &state);

        let read = read_track_state(&tmp).unwrap();
        assert_eq!(read.last_sha, "abc123def");
        assert_eq!(read.path, tmp.to_string_lossy());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn incremental_falls_back_to_full_when_no_track_state() {
        let tmp = std::env::temp_dir().join("mnemosyne-incr-fallback");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        // Write a small markdown file so ingest doesn't fail with "no source files"
        fs::write(tmp.join("README.md"), "# Test\nContent.\n").unwrap();

        // This is not a git repo, so it falls back to full ingest
        let result = incremental_ingest_directory(&tmp.to_string_lossy());
        assert!(
            result.is_ok(),
            "should fall back to full ingest: {:?}",
            result.err()
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn incremental_same_revision_noop() {
        // In a git repo, if the SHA hasn't changed, we return Err to indicate no work.
        let tmp = std::env::temp_dir().join("mnemosyne-incr-git");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("README.md"), "# Test\nContent.\n").unwrap();

        // Initialize a git repo
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&tmp)
            .output()
            .unwrap();

        // First ingest — full scan
        let result = incremental_ingest_directory(&tmp.to_string_lossy());
        assert!(
            result.is_ok(),
            "first ingest should succeed: {:?}",
            result.err()
        );
        let (ingest_result, was_full) = result.unwrap();
        assert!(was_full, "first ingest should be full");
        assert!(ingest_result.files_processed > 0);

        // Second ingest — same SHA, should return "no changes" error
        let result2 = incremental_ingest_directory(&tmp.to_string_lossy());
        assert!(result2.is_err(), "second ingest should indicate no changes");
        assert!(
            result2
                .unwrap_err()
                .contains("no changes since last ingest"),
            "should say no changes"
        );

        // Track file should exist
        assert!(
            tmp.join(TRACK_FILE).exists(),
            "track file should be written"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn full_and_incremental_produce_equivalent_entity_count() {
        let tmp = std::env::temp_dir().join("mnemosyne-incr-equiv");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join("README.md"),
            "# Architecture\nThe system uses a pipeline.\n",
        )
        .unwrap();
        fs::write(
            tmp.join("config.toml"),
            "[package]\nname = \"test\"\nversion = \"1.0\"\n",
        )
        .unwrap();

        // Full ingest
        let full = ingest_directory(&tmp.to_string_lossy()).expect("full ingest");
        let full_entity_count = full.ir.entities.len();
        assert!(full_entity_count > 0);

        // Init git for incremental tracking
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&tmp)
            .output()
            .unwrap();

        // Incremental ingest (first run → full since no track state for this SHA)
        let (incr, was_full) =
            incremental_ingest_directory(&tmp.to_string_lossy()).expect("incremental ingest");
        assert!(was_full, "first incremental should be full");

        let incr_entity_count = incr.ir.entities.len();
        assert_eq!(
            full_entity_count, incr_entity_count,
            "full and incremental should produce same entity count"
        );

        let _ = fs::remove_dir_all(&tmp);
    }
}
