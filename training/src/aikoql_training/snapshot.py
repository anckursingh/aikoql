"""Snapshot adapter (design Phase 2 + §10).

Dataset generation is snapshot-based: a captured state (database
identity + knowledge revision + configuration + seed) that every
example's source block traces to. The identity is content-derived —
two snapshots of the same immutable state produce the same
snapshot_id; created_at is metadata, not identity.

The knowledge revision comes from the public health tool
(journal_seq + audit_hash). database_id is an explicit operator
parameter — the server exposes no database identity (recon: the MCP
initialize handshake carries only serverInfo {name, version}).
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Dict, Optional

from aikoql_training.models import (
    GENERATOR_VERSION,
    SCHEMA_VERSION,
    to_json,
)


def _sha256(blob: str) -> str:
    return "sha256:" + hashlib.sha256(blob.encode("utf-8")).hexdigest()


@dataclass(frozen=True)
class Snapshot:
    snapshot_id: str
    database_id: str
    knowledge_revision: str
    schema_version: str
    generator_version: str
    seed: int
    created_at: str
    source_manifest_hash: str
    configuration_hash: str

    def to_dict(self) -> Dict[str, Any]:
        return {
            "snapshot_id": self.snapshot_id,
            "database_id": self.database_id,
            "knowledge_revision": self.knowledge_revision,
            "schema_version": self.schema_version,
            "generator_version": self.generator_version,
            "seed": self.seed,
            "created_at": self.created_at,
            "source_manifest_hash": self.source_manifest_hash,
            "configuration_hash": self.configuration_hash,
        }


def capture_snapshot(
    database_id: str,
    health: Dict[str, Any],
    *,
    config: Optional[Dict[str, Any]] = None,
    source_manifest: Any = None,
    seed: int = 0,
    now: Optional[str] = None,
) -> Snapshot:
    """Capture a dataset snapshot from a health payload (the public
    health tool). Fail-closed on identity inputs: an empty database_id
    or a health payload missing the revision fields is refused, not
    defaulted."""
    if not isinstance(database_id, str) or not database_id.strip():
        raise ValueError("database_id must be a non-empty string")
    for key in ("journal_seq", "audit_hash"):
        if key not in health:
            raise ValueError(f"health payload missing {key}")
    knowledge_revision = f"{health['journal_seq']}:{health['audit_hash']}"
    identity = to_json(
        {"database_id": database_id, "knowledge_revision": knowledge_revision}
    )
    return Snapshot(
        snapshot_id=_sha256(identity),
        database_id=database_id,
        knowledge_revision=knowledge_revision,
        schema_version=SCHEMA_VERSION,
        generator_version=GENERATOR_VERSION,
        seed=seed,
        created_at=now or datetime.now(timezone.utc).isoformat(),
        source_manifest_hash=_sha256(to_json({} if source_manifest is None else source_manifest)),
        configuration_hash=_sha256(to_json({} if config is None else config)),
    )


def snapshot_to_json(snapshot: Snapshot) -> str:
    """Canonical single-line serialization (sort-keyed, separator-tight)."""
    return to_json(snapshot.to_dict())
