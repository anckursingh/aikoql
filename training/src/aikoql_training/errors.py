"""Typed errors (design §28 — the seed set for the training engine)."""


class TrainingDataError(Exception):
    """Base class for every aikoql-training error."""


class SchemaError(TrainingDataError):
    """A training example violates the canonical schema (fail-closed)."""


class DatasetError(TrainingDataError):
    """A dataset on disk is missing, tampered or malformed (fail-closed)."""

