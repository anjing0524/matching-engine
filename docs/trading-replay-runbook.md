# Core replay runbook

Use this when a core process restarts or is rebuilt from a snapshot.

## Inputs

- versioned snapshot JSON produced by `TradingCore::save_snapshot`
- command log segment starting **after** the snapshot's last command
- contract symbols expected by the snapshot

The snapshot includes a CRC32 checksum; do not bypass validation.

## Restore sequence

1. Stop the middleware from admitting new commands.
2. Place the snapshot and replay log on the core host. Middleware writes
   accepted commands to fixed 256-byte slots, either one segment via
   `--command-log` or size-bounded rotated segments via `--command-log-dir`.
3. Start the core with the snapshot:

```text
trading-core \
  --command-ring /run/trading/command.ring \
  --public-ring /run/trading/public.ring \
  --private-ring /run/trading/private.ring \
  --snapshot /var/lib/trading/core.snapshot.json
```

4. Replay the post-snapshot command log with
   `trading::snapshot::replay::replay_from_snapshot`.
5. Compare the replayed core snapshot with the expected snapshot if one is
   available; reject on mismatch.
6. Admit commands only after replay completes.
7. Verify public/private sequence continuity and that private replay serves
   reconnecting clients.

## Failure handling

- Snapshot checksum/version failure → do not serve; restore an older snapshot.
- Missing command-log segment → full replay from the last complete snapshot.
- Core crash during replay → repeat from the same snapshot; replay is
  deterministic.
- Never truncate or overwrite the only snapshot copy; keep at least two
  generations.

## Operational checks

- `trading_core` process is running before gateway resumes admitting orders.
- command ring is empty at cutover.
- metrics show commands increasing and no rejected-command spike.
- public sequence continues without a gap; NACK repair remains available.

## Full runtime snapshot

`trading-engine run --snapshot <path>` restores a `TradingSnapshot` containing the
core book/business state and, when configured, combo queue/leg/positions/margin
state. The file is versioned and CRC32-checked, and is written with atomic
replacement by `TradingSnapshot::save`.

Use the full runtime snapshot when combo and core state must be recovered
together; use the split `trading-core --snapshot` path only for core-only
recovery.
