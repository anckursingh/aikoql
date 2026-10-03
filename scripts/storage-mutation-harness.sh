#!/usr/bin/env bash
# F-03 (PR #7 fuzz review §12) — mutation harness for the storage engine
# itself. The review's rule: a surviving mutant is a test-suite defect.
# Each mutation applies ONE surgical damage to a detached-worktree copy of
# the tree (HEAD) and runs the named regression that must kill it. cargo's
# exit code propagates:
#
#   non-zero = the mutant IS killed (that exit is the RED the archive
#              captures via scripts/red-archive.sh)
#   zero     = SURVIVED — the estate has a hole, not a success
#
#   storage-mutation-harness.sh <id>   apply one mutation, run its killer
#   storage-mutation-harness.sh all    run all; exit 0 iff all are killed
#
# The killers are existing regressions — the review's acceptance is "every
# selected mutant is killed by at least one named regression", not new
# tests. Four mutants re-aim at the estate's actual crash windows: the
# flush funnel parks only at after_identity (there is no park between
# manifest and WAL truncate), so the review's "WAL truncation moved before
# manifest" lands at that window (m-s8); the "CURRENT publication
# reordered" mutant targets the COMPACT funnel where ci007 parks after
# CURRENT (m-s7); m-s2 lands on the !in_set stale-checksum branch (the
# corruption tests re-stamp block checksums); m-s6 lands on the PR6-002
# delta-coverage validator. Two more were witness corrections after real
# survivors: m-s1's guard sits at THREE indent sites (12/8/4-space —
# block_entries, block_get_v2, scan_seek_pos) and the first pattern set
# missed the 12-space one, so its corruption class kept failing closed;
# m-s10's first killer pinned allocs on a fixture with no object rows —
# the semantic pin (scan == byte oracle across layers WITH object rows)
# is the witness that actually fails.
#
# CARGO_TARGET_DIR is shared with the main tree so dependency artifacts
# are reused — only the mutated crate + its test binary rebuild per run.
# CONSEQUENCE: the mutant's deps binaries land in the shared target dir and
# a later LIVE-tree cargo run can reuse them (mtime confusion) — the mutant
# then "panics on clean code". After any harness run, `cargo clean -p
# aikoql-storage-v2` before live verification.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
crate="$root/crates/storage/aikoql-v2"

mutations() {
  echo "m-s1-restart-count m-s2-checksum m-s3-tombstone m-s4-newest-wins \
m-s5-duplicate-guard m-s6-delta-coverage m-s7-current-before-manifest \
m-s8-wal-truncate-early m-s9-cache-transparency m-s10-scan-skip \
m-s11-placement-direct"
}

# pymut <file> <python-code> — the multi-line mutations are python string
# surgery (sed can't span the funnel blocks); single-line ones ride python
# too so every mutation is one mechanism.
pymut() {
  f="$1"
  code="$2"
  # PYTHONUTF8: the anchors carry §/em-dash bytes — a cp1252 stdin decode
  # silently mangles them and every replace no-ops (the assert catches it).
  # NOTE: $code arrives via parameter expansion, which SKIPS the heredoc's
  # backslash collapse — \\n in the code reaches python as a literal
  # backslash-n (m-s8's first "kill" was this crash, exit 1). Anchors must
  # be triple-quoted real newlines, never backslash escapes.
  PYTHONUTF8=1 python - "$f" <<PY
import sys
f = sys.argv[1]
s = open(f, encoding="utf-8").read()
orig = s
$code
assert s != orig, "mutation did not apply — anchor moved or drifted"
open(f, "w", encoding="utf-8", newline="\n").write(s)
PY
  # a pymut crash is a harness error (exit 3), NEVER a kill (1/101): under
  # the all-loop's `if mutate` context set -e is suppressed, so the crash
  # would otherwise fall through to the killer on UNMUTATED source and read
  # as a survivor — or, in single mode, as a fake kill.
  [ $? -eq 0 ] || { echo "pymut failed for $id" >&2; exit 3; }
}

# killer <binary> <test-fn> — the named regression, run in the mutated
# worktree. Non-zero exit = the mutant is killed.
killer() {
  bin="$1"
  test="$2"
  (cd "$crate" && CARGO_TARGET_DIR="$root/target" \
    cargo test --test "$bin" "$test" -- --exact 2>&1 | tail -15)
}

mutate() {
  id="$1"
  tmp="$(mktemp -d)"
  tree="$tmp/tree"
  trap 'git worktree remove --force "$tree" 2>/dev/null || true; rm -rf "$tmp"' EXIT
  git worktree add --detach "$tree" HEAD >/dev/null 2>&1
  # the crate path inside the worktree:
  crate="$tree/crates/storage/aikoql-v2"
  case "$id" in
    m-s1-restart-count)
      # the v2 restart-count plausibility guard is deleted at all THREE
      # indent sites (block_get_v2 8-space, block_entries 12-space,
      # scan_seek_pos 4-space) — each count-asserted so an anchor drift is
      # a harness error, not a silent survivor
      pymut "$crate/src/segment.rs" '
p12 = """            let mut table_len = 6 + 4 * restarts;
            if table_len > payload.len() {
                return Err(FormatError::Corrupt(
                    \"v2 restart table exceeds payload\".into(),
                ));
            }
"""
p8 = """        let mut table_len = 6 + 4 * restarts;
        if table_len > payload.len() {
            return Err(FormatError::Corrupt(
                \"v2 restart table exceeds payload\".into(),
            ));
        }
"""
p4 = """    let mut table_len = 6 + 4 * restarts;
    if table_len > payload.len() {
        return Err(FormatError::Corrupt(
            \"v2 restart table exceeds payload\".into(),
        ));
    }
"""
assert s.count(p12) == 1, "p12 anchor drift"
assert s.count(p8) == 1, "p8 anchor drift"
assert s.count(p4) == 1, "p4 anchor drift"
s = s.replace(p12, """            let mut table_len = 6 + 4 * restarts;""")
s = s.replace(p8, """        let mut table_len = 6 + 4 * restarts;""")
s = s.replace(p4, """    let mut table_len = 6 + 4 * restarts;""")'
      killer restart_corruption every_corruption_class_fails_closed
      ;;
    m-s2-checksum)
      # the stale-checksum fail-closed on an unknown block version is
      # deleted — damage now reads as Unsupported (a future format)
      pymut "$crate/src/segment.rs" '
s = s.replace("""                if checksum8(&sk) != stored {
                    return Err(FormatError::Corrupt(format!(
                        \"block version {version} damaged\"
                    )));
                }
""", "", 1)'
      killer block_v2 block_v2_future_version_fails_closed
      ;;
    m-s3-tombstone)
      # the tombstone predicate drops — a delete winner publishes as a
      # live row instead of retiring its replica group
      pymut "$crate/src/compaction.rs" '
s = s.replace("if fresh && entry.flags & FLAG_DELETE == 0 {", "if fresh {", 1)'
      killer winner_matrix tombstone_winner_retires_only_its_replica_and_can_resurrect
      ;;
    m-s4-newest-wins)
      # the merge heap's seq ordering reverses — the OLDEST version of a
      # key drains first and wins
      pymut "$crate/src/compaction.rs" '
s = s.replace("(Reverse(&self.entry.key), self.entry.seq, self.idx).cmp(&(",
              "(Reverse(&self.entry.key), Reverse(self.entry.seq), self.idx).cmp(&(", 1)'
      killer winner_matrix winner_matrix_in_one_segment
      ;;
    m-s5-duplicate-guard)
      # the shared duplicate-(key,seq) guard is deleted — both publish
      # entries accept a corpus they must reject
      pymut "$crate/src/segment.rs" '
s = s.replace("""            return Err(FormatError::Invalid(\"duplicate (key, seq) pair\".into()));
""", "", 1)'
      killer flush_equivalence duplicate_corpora_are_rejected_identically
      ;;
    m-s6-delta-coverage)
      # the PR6-002 coverage validator (the fail-closed generation check
      # at open) is deleted — a missing post-checkpoint delta opens clean
      pymut "$crate/src/db.rs" '
s = s.replace("""        validate_delta_coverage(&config.dir, checkpoint.as_ref(), &manifest)?;
""", "", 1)'
      killer delta_coverage missing_post_checkpoint_delta_fails_closed
      ;;
    m-s7-current-before-manifest)
      # the COMPACT funnel's manifest publication moves past CURRENT — at
      # ci007's FAIL_AFTER_PUBLISH park CURRENT names a manifest that is
      # not durable yet
      pymut "$crate/src/db.rs" '
s = s.replace("""    // SE2-M36 — staged: the §38 MANIFEST windows park inside.
    Manifest::publish_staged(
        &manifest_path(&config.dir, st.generation),
        &manifest,
        Some(\"MANIFEST\"),
    )?;
    crash_park(\"AIKOQL_V2_COMPACT_PARK\", &config.dir, \"after_manifest\");
""", "", 1)
s = s.replace("""    crash_park(\"AIKOQL_V2_PLACE_PARK\", &config.dir, \"FAIL_AFTER_PUBLISH\");
""", """    crash_park(\"AIKOQL_V2_PLACE_PARK\", &config.dir, \"FAIL_AFTER_PUBLISH\");
    // MUTATED (m-s7): manifest publication moved past CURRENT
    Manifest::publish_staged(
        &manifest_path(&config.dir, st.generation),
        &manifest,
        Some(\"MANIFEST\"),
    )?;
    crash_park(\"AIKOQL_V2_COMPACT_PARK\", &config.dir, \"after_manifest\");
""", 1)'
      killer crash_injection ci007_fail_after_publish
      ;;
    m-s8-wal-truncate-early)
      # the FLUSH funnel's WAL truncate moves BEFORE the after_identity
      # park (before the manifest) — at fl004's window an acked row lives
      # nowhere: orphan segment, no manifest, truncated WAL
      pymut "$crate/src/db.rs" '
start = s.index("""        {
            let mut wal = wal.lock().unwrap();""")
sync = s.index("\"WAL sync: {e}\"", start)
end = s.index("""
        }
""", sync) + len("""
        }
""")
block = s[start:end]
s = s[:start] + s[end:]
s = s.replace("""        crash_park(\"AIKOQL_V2_FLUSH_PARK\", &config.dir, \"after_identity\");
""", block + """        crash_park(\"AIKOQL_V2_FLUSH_PARK\", &config.dir, \"after_identity\");
""", 1)'
      killer flush_identity fl004_crash_before_flush_publication_preserves_old_state
      ;;
    m-s9-cache-transparency)
      # a cache hit serves zeroed bytes — the block decode fails or
      # yields garbage where the uncached path answers correctly
      pymut "$crate/src/segment.rs" '
s = s.replace("            let hit = cache.get(self.cache_id, i as u32);",
              "            let hit = cache.get(self.cache_id, i as u32).map(|raw| std::sync::Arc::new(vec![0u8; raw.len()]));", 1)'
      killer cache cache_never_changes_answers
      ;;
    m-s10-scan-skip)
      # the byte-surface filter is deleted — object rows answer a byte
      # scan; the killer is the SEMANTIC pin (scan == byte oracle across
      # layers with object rows), not the alloc pin (its fixture holds no
      # object rows, so the filter never fires there)
      pymut "$crate/src/segment.rs" '
s = s.replace("""            if replica_id != ReplicaId(0) {
                continue;
            }
""", "", 1)'
      killer prefix_scan_oracle tdd026_scan_matches_the_byte_oracle_across_layers_with_object_rows
      ;;
    m-s11-placement-direct)
      # the placement direct read ignores its entry offset — the anchor
      # read answers with the block's first entry
      pymut "$crate/src/segment.rs" '
s = s.replace("Ok(entries.into_iter().nth(entry_offset as usize))",
              "Ok(entries.into_iter().nth(0))", 1)'
      killer placement_equivalence object_placement_and_direct_read_agree_through_every_stage
      ;;
    *)
      echo "unknown mutation: $id" >&2
      exit 2
      ;;
  esac
}

case "${1:-}" in
  all)
    fail=0
    for m in $(mutations); do
      if mutate "$m"; then
        echo "SURVIVED: $m" >&2
        fail=1
      else
        echo "killed: $m"
      fi
    done
    if [ $fail -ne 0 ]; then
      echo "storage mutation harness: a mutant survived its killer" >&2
      exit 1
    fi
    echo "storage mutation harness: all eleven killed"
    ;;
  "")
    echo "usage: storage-mutation-harness.sh <id>|all" >&2
    exit 2
    ;;
  *)
    mutate "$1"
    ;;
esac
