"""Typed errors (design §28 — the seed set for the training engine).

Every error carries four optional category fields — stage (the
pipeline stage that raised), scenario (the scenario_id), code (a
stable machine-readable code) and example_id — and renders them as a
JSON-serializable info dict via to_info(). The fields are optional at
construction: schema validation raises (the models.py set) carry
none; the pipeline raises (cli.py) carry the ones their stage knows.
"""


class TrainingDataError(Exception):
    """Base class for every aikoql-training error."""

    def __init__(self, message, *, stage=None, scenario=None, code=None,
                 example_id=None):
        super().__init__(message)
        self.stage = stage
        self.scenario = scenario
        self.code = code
        self.example_id = example_id

    def to_info(self):
        return {
            "message": str(self),
            "stage": self.stage,
            "scenario": self.scenario,
            "code": self.code,
            "example_id": self.example_id,
        }


class SchemaError(TrainingDataError):
    """A training example violates the canonical schema (fail-closed)."""


class DatasetError(TrainingDataError):
    """A dataset on disk is missing, tampered or malformed (fail-closed)."""
