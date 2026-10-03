"""Dataset layer (design Phase 13+14): splitter + writer/reader."""

from aikoql_training.dataset.splitter import assign_splits
from aikoql_training.dataset.writer import read_dataset, write_dataset

__all__ = ["assign_splits", "write_dataset", "read_dataset"]
