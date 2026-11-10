# Combo atomicity and rollback evaluation

## Current algorithm

For a two-leg combination:

1. check best prices and spread
2. check aggregate side quantity in O(1)
3. snapshot both populated leg-book levels into reusable buffers
4. sweep leg1 and leg2
5. if either leg under-fills, or worst realized spread violates the limit,
   restore both books from the snapshots
6. otherwise publish the combo fill

Snapshots are taken **after** the cheaper prechecks, so missing-liquidity and
spread-only failures do not pay the snapshot cost.

## Measured cost

On M4, with a leg book holding a single order:

```text
combo atomic fills   ≈ 1.25M fill/s
snapshot only        ≈ 3 ns/snapshot
```

Snapshot traversal uses tracked min/max indices and the price bitmap, so it
visits only populated levels rather than every tick.

## Alternatives considered

### No rollback, rely on precheck

Fastest, because aggregate side quantity and price bounds guarantee a sweep can
fill the requested quantity. Rejected because any internal book bug would leave
leg risk with no recovery path; the trading prioritizes correctness.

### Journal-based undo

Record consumed nodes and FIFO links, then reverse on failure. Lower allocation
than a snapshot but significantly more complex, and wrong journal ordering can
silently break price/time priority. Not adopted yet.

### Two-phase reserve/commit

Reserve quantity on both legs, then commit. Would need a first-class book
reservation API and a clear policy for partially filled resting orders. This is
the preferred long-term direction if snapshot cost becomes material.

## Decision

Keep snapshot + restore. Current cost is small relative to the atomic fill path,
buffers are reused, and the correctness story is simple and testable.

Revisit if profiling shows snapshots above ~20% of combo execution time or if
leg books become large enough that populated-level traversal dominates.
