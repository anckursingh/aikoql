# Group Commit Throughput Matrix — SE2-M6

Generated only when `SE2M6_NIGHTLY=1` (strict opt-in). Perf numbers are
report cells, never asserts — the report regenerates only with the env set.

- Test: `group_commit_throughput_matrix`
- Build mode: release
- Workload: 200 single-op batches, 128-byte values, 1 MiB+ memtable (no flush during the run)

- Sync, 1 writer, 200 batches: 229 ms, 200 fsyncs
- GroupCommit, 1 writer, wait=0, 200 batches: 233 ms, 200 fsyncs
- GroupCommit, 8 writers × 25 batches, wait=5ms: 3131 ms, 200 fsyncs
