#!/usr/bin/env bash
# PR6-R3-005 — docs/PR6-TDD-DISPOSITIONS.md records evidence verified at a
# specific head; its last-modified commit is the stamp. Any change that
# moves the reviewed tip must re-stamp the doc in the same change, or CI
# fails here — a reviewer must never read evidence claimed at an older head.
#
# The tip under review: on a two-parent HEAD (a PR merge ref, or a pushed
# merge commit — this repo merges with merge commits) it is the second
# parent; on a plain push it is HEAD itself.
#
# RED proof (the R2-012 pattern): `bash scripts/check-disposition-head.sh
# origin/main` exits 1 — origin/main has no stamp for the doc at all (the
# file exists only in unpushed commits); against the pre-stamp branch head
# it exits 1 on stamp f7696be vs reviewed tip f291130 — the review's exact
# staleness scenario. GREEN: against the re-stamped head.
set -u
ref="${1:-HEAD}"
head=$(git rev-parse "$ref") || exit 2
parents=$(git rev-list --parents -n 1 "$head" | awk '{print NF-1}')
if [ "$parents" -eq 2 ]; then
  tip=$(git rev-parse "$ref^2") || exit 2
else
  tip="$head"
fi
doc=docs/PR6-TDD-DISPOSITIONS.md
stamp=$(git log -1 --format=%H "$ref" -- "$doc") || exit 2
if [ "$stamp" != "$tip" ]; then
  echo "DISPOSITIONS STALE: $doc last modified by $stamp, but the reviewed tip is $tip"
  echo "re-stamp the dispositions doc in the same change that moved the tip"
  exit 1
fi
