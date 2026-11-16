# ADR-0005: Level storage stays specialized; share the order lifecycle

**Status:** Accepted  
**Date:** 2026-11-13

## Context

ADR-0003 Phase 2 proposed one generic `PriceLevelBook` for ordinary and combo
orders. The two books have different constraints:

```text
ordinary Contract:
  bounded u64 price range
  dense array + FastBitmap + intrusive slab
  O(1) price index, benchmark-critical

Combo Pair:
  signed i64 spread, sparse/unbounded
  BTreeMap price levels + FIFO queues
  low order count, allocation flexibility
```

Forcing both into one storage strategy would either make the core sparse/slower
or make combo allocate a dense range it cannot bound.

## Decision

Do not unify the price-level storage strategy. Share only:

- order identity (`OrderId`, account/session/client ids)
- order state transitions
- atomic fill planning/execution protocol
- event projection

The core keeps its dense order book. The combo book keeps a sparse signed
spread ladder. A common `OrderLifecycle` vocabulary is the seam, not a common
`PriceLevelBook` container.

## Consequences

- Phase 2 is revised: no generic price-level container.
- Phase 3 (one VenueRunner) and Phase 4 (Venue/ComboVenue ownership) remain.
- Any future storage unification must pass the 500k/s pinned benchmark first.
