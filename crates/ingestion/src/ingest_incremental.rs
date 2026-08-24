//! MRFC-0070 R10.1: Incremental directory ingestion.
//!
//! Tracks the last git commit SHA in a marker file. On re-ingestion, only
//! files changed since the last commit are re-parsed. Unchanged files retain
//! their entities; deleted files have their entities flagged as stale;
//! renamed files (git -M detection) keep entity identity — the old path's
//! entities are dropped and the re-compiled new path takes over, so no
//! ghost duplicates (merge keys entities on document_id).
//!
//! Pipeline: git diff -M → changed files + rename pairs → parse changed →
//! reconcile → merge.
//! Reuses the existing `reconcile()`, `merge_knowledge_ir()`, and
//! `compile_file()` infrastructure.

use crate::ingest_dir::{compile_file, current_head_sha, IngestResult};
use crate::ir::*;
use crate::merge::merge_knowledge_ir;
use crate::reconcile::{reconcile, source_matches};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Tracking state persisted between incremental runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackState {
    pub path: String,
    pub last_sha: String,
}

const TRACK_FILE: &str = ".aikoql-track.json";

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

/// One rename pair from `git diff -M`. Only content-identical (R100) pairs
/// reach the caller; an edited rename is versioned like a modify — its new
/// path is emitted as a plain path instead.
#[derive(Debug)]
struct RenamePair {
    old: String,
    new: String,
}

/// Get changed files and rename pairs between two commits.
///
/// Returns `(plain_paths, renames)`. Rename pairs are detected with git's
/// `-M` similarity heuristic; `plain_paths` includes the new path of an
/// edited rename (so its facts get reconciled like a modify).
fn git_change_set(
    root: &Path,
    from_sha: &str,
    to_sha: &str,
) -> Result<(Vec<String>, Vec<RenamePair>), String> {
    let range = format!("{}..{}", from_sha, to_sha);
    // R4: propagate git failures — an empty change set here silently served
    // stale IR as current, so failures must surface to the caller (which
    // falls back to full ingest).
    let output = Command::new("git")
        .args(["diff", "-M", "--name-status", &range])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git diff failed: {}", e))?;
    if !output.status.success() {
        return Err("git diff exited non-zero".into());
    }
    let mut plain = Vec::new();
    let mut renames = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (status, rest) = line.split_once('\t').unwrap_or((line, ""));
        if let Some((old, new)) = rest.split_once('\t') {
            if status == "R100" {
                renames.push(RenamePair {
                    old: old.to_string(),
                    new: new.to_string(),
                });
            } else {
                // Edited rename: reconcile the new path like a modify.
                plain.push(new.to_string());
            }
        } else {
            plain.push(rest.to_string());
        }
    }
    Ok((plain, renames))
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
    let (mut changed, renames) = git_change_set(path, &prev.last_sha, &head_sha)?;

    // Never ingest our own tracking state — it lives in the repo root and
    // would be compiled as a text entity if a user commits it.
    changed.retain(|f| f != TRACK_FILE);

    if changed.is_empty() && renames.is_empty() {
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

    // Filter to files that exist and aren't skipped (rename old-halves are
    // gone from disk and drop out here)
    let existing: Vec<PathBuf> = changed
        .iter()
        .chain(renames.iter().map(|r| &r.new))
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

    // Rename identity: drop the old path's entities from the previous IR so
    // merge (keyed on document_id) yields one entity at the new path instead
    // of a ghost duplicate. Facts survive via statement dedup; relations are
    // name-based and re-attach to the surviving entity.
    let mut prev_ir = previous_ir.clone();
    prev_ir.entities.retain(|e| {
        let doc = e.evidence.document_id.as_deref().unwrap_or("");
        !renames.iter().any(|r| source_matches(doc, &r.old))
    });

    // Merge new IRs with previous (existing entities from unchanged files persist)
    let mut all_irs: Vec<KnowledgeIr> = new_irs;
    all_irs.push(prev_ir);
    let mut merged = merge_knowledge_ir(&all_irs);
    merged.document_id = Some(format!("ingest-dir:{}", root));
    // KB-009 versioned manifest: the merged IR carries the revision it
    // reflects, matching the tracking file.
    merged.source_revision = Some(head_sha.clone());
    merged.extractor = "ingest-dir-incremental".into();
    merged.page_count = existing.len() as u32;

    // Reconcile to flag stale facts from changed/deleted files. Pure renames
    // stay out of this list — content-identical, so nothing is stale.
    let report = reconcile(&changed, &merged);
    for fact in &report.potentially_stale_facts {
        merged.facts.push(FactCandidate {
            snippet: None,
            statement: format!("[STALE] {}", fact),
            entities: vec![],
            confidence: 0.1,
            evidence: Evidence {
                document_id: merged.document_id.clone(),
                page: None,
                source: None,
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
        let tmp = std::env::temp_dir().join("aikoql-track-test");
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
        let tmp = std::env::temp_dir().join("aikoql-incr-fallback");
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
        // KB-009: no git repo → no revision in the manifest
        assert!(
            result.unwrap().0.ir.source_revision.is_none(),
            "non-git ingest must not stamp a source revision"
        );
        assert!(
            !tmp.join(TRACK_FILE).exists(),
            "no tracking manifest for a non-git dir"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn incremental_same_revision_noop() {
        // In a git repo, if the SHA hasn't changed, we return Err to indicate no work.
        let tmp = std::env::temp_dir().join("aikoql-incr-git");
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
        let tmp = std::env::temp_dir().join("aikoql-incr-equiv");
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

    #[test]
    fn manifest_carries_revision_and_updates() {
        // KB-009 versioned manifest: the ingested IR carries the git revision
        // it reflects (same SHA as the tracking file), and a new commit
        // updates it. Determinism: re-ingesting the same revision is a no-op.
        let tmp = std::env::temp_dir().join("aikoql-incr-manifest");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join("README.md"),
            "# Manifest\nKnowledge reflects a revision.\n",
        )
        .unwrap();

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

        let sha_a = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        let sha_a = String::from_utf8(sha_a.stdout).unwrap();
        let sha_a = sha_a.trim();

        // Full ingest at revision A
        let (full, was_full) =
            incremental_ingest_directory(&tmp.to_string_lossy()).expect("full ingest");
        assert!(was_full);
        assert_eq!(
            full.ir.source_revision.as_deref(),
            Some(sha_a),
            "manifest must record the ingested revision"
        );
        let track = read_track_state(&tmp).expect("track file written");
        assert_eq!(
            track.last_sha, sha_a,
            "tracking manifest and IR revision must agree"
        );

        // New commit → incremental ingest carries the new revision
        fs::write(
            tmp.join("README.md"),
            "# Manifest\nKnowledge reflects a newer revision.\n",
        )
        .unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        assert!(std::process::Command::new("git")
            .args(["commit", "-m", "rev B"])
            .current_dir(&tmp)
            .output()
            .unwrap()
            .status
            .success());

        let sha_b = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        let sha_b = String::from_utf8(sha_b.stdout).unwrap();
        let sha_b = sha_b.trim();
        assert_ne!(sha_a, sha_b, "test setup: distinct revisions");

        let (incr, _) = incremental_diff_ingest(&tmp.to_string_lossy(), &full.ir)
            .expect("incremental ingest at revision B");
        assert_eq!(
            incr.ir.source_revision.as_deref(),
            Some(sha_b),
            "incremental manifest must advance to the new revision"
        );

        // Determinism: same-SHA re-ingest is a no-op (INC-001)
        let again = incremental_ingest_directory(&tmp.to_string_lossy());
        assert!(
            again.is_err() && again.unwrap_err().contains("no changes"),
            "same-SHA re-ingest should be a no-op"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn git_change_set_propagates_git_failure() {
        // R4: git failure must surface as Err, not an empty change set —
        // an empty set would silently serve stale IR as current.
        let tmp = std::env::temp_dir().join("aikoql-gitdiff-fail");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Not a git repository → git exits non-zero.
        let err = git_change_set(&tmp, "HEAD~1", "HEAD").unwrap_err();
        assert!(!err.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rename_preserves_entity_identity() {
        // INC-003: `git mv` a source file → the entity keeps its identity:
        // same name set, one entity (no ghost at the old path), evidence path
        // updated, unchanged facts survive once, no spurious [STALE] flags.
        let tmp = std::env::temp_dir().join("aikoql-incr-rename");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join("engine.md"),
            "# ConstraintEngine\nThe engine validates constraints at commit time.\n",
        )
        .unwrap();

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

        // Full ingest (first run)
        let (full, was_full) =
            incremental_ingest_directory(&tmp.to_string_lossy()).expect("full ingest");
        assert!(was_full, "first ingest should be full");
        let before_names: std::collections::BTreeSet<String> =
            full.ir.entities.iter().map(|e| e.name.clone()).collect();
        assert!(
            before_names.contains("ConstraintEngine"),
            "fixture entity missing: {:?}",
            before_names
        );
        let before_entity_count = full.ir.entities.len();
        let before_stale = full
            .ir
            .facts
            .iter()
            .filter(|f| f.statement.starts_with("[STALE]"))
            .count();
        assert_eq!(before_stale, 0);

        // Rename the file (git mv auto-stages; commit without add -A so the
        // untracked track file stays out of the diff)
        assert!(std::process::Command::new("git")
            .args(["mv", "engine.md", "core-engine.md"])
            .current_dir(&tmp)
            .output()
            .unwrap()
            .status
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "-m", "rename engine.md"])
            .current_dir(&tmp)
            .output()
            .unwrap()
            .status
            .success());

        // Incremental ingest across the rename
        let (incr, stale) = incremental_diff_ingest(&tmp.to_string_lossy(), &full.ir)
            .expect("incremental ingest after rename");

        // Identity: same entity set, same count — no ghost at the old path
        let after_names: std::collections::BTreeSet<String> =
            incr.ir.entities.iter().map(|e| e.name.clone()).collect();
        assert_eq!(
            after_names, before_names,
            "rename must keep the identity set"
        );
        assert_eq!(
            incr.ir.entities.len(),
            before_entity_count,
            "rename must not duplicate entities"
        );

        // Evidence path follows the new location
        let engine = incr
            .ir
            .entities
            .iter()
            .find(|e| e.name == "ConstraintEngine")
            .expect("entity survived the rename");
        assert!(
            engine
                .evidence
                .document_id
                .as_deref()
                .map(|d| d.ends_with("core-engine.md"))
                .unwrap_or(false),
            "evidence path should update to the new location, got {:?}",
            engine.evidence.document_id
        );

        // Pure rename: nothing is stale, no [STALE] facts appended
        assert!(stale.is_empty(), "pure rename flags nothing stale");
        let after_stale = incr
            .ir
            .facts
            .iter()
            .filter(|f| f.statement.starts_with("[STALE]"))
            .count();
        assert_eq!(after_stale, 0, "no [STALE] facts on a pure rename");

        // Deterministic: re-ingesting the same revision is a no-op
        let again = incremental_ingest_directory(&tmp.to_string_lossy());
        assert!(
            again.is_err() && again.unwrap_err().contains("no changes"),
            "same-SHA re-ingest should be a no-op"
        );

        let _ = fs::remove_dir_all(&tmp);
    }
}
