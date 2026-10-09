"""Dataset layer (design Phase 13+14): splitter + writer/reader +
config (FZ-T3) + gates (§26)."""

from aikoql_training.dataset.config import DEFAULT_CONFIG, load_config
from aikoql_training.dataset.gates import validate_dataset
from aikoql_training.dataset.splitter import assign_splits
from aikoql_training.dataset.writer import read_dataset, write_dataset

__all__ = ["assign_splits", "write_dataset", "read_dataset",
           "load_config", "DEFAULT_CONFIG", "validate_dataset"]
