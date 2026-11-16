# ADR-0003: Consolidate on one venue model

**Status:** Accepted  
**Date:** 2026-11-13

## Context

The system grew feature-by-feature. The same concepts have parallel
representations (orders, books, events, runners, snapshots), so each feature
adds another local model. The code is not primarily too long; it is incoherent.
A futures exchange core needs explicit ownership for matching determinism,
sequence domains, risk, recovery, and audit.

## Decision

Adopt the target model in `docs/system-architecture.md`:

```text
Venue
  InstrumentMatching
    PriceLevelBook
    RiskState
  ComboPair
  EventProjector
  Snapshot/Replay
```

Consolidate by strangler migration, never a big-bang rewrite:

1. domain newtypes + per-stream canonical events (ADR-0004)
2. one generic `PriceLevelBook` for core and combo
3. one `VenueRunner` for single and split deployment
4. split `Venue` / `ComboVenue` / `RiskState` / `EventProjector`
5. one snapshot envelope with registered schema versions

Every phase must keep the current command semantics and benchmark at or above
the 500k/s target.

## Consequences

- New features wait until phase 1's event/order identity is agreed.
- Temporary adapters from existing `CoreEvent`/`ComboEvent` are acceptable only
  during a phase and must be removed at its end.
- The architecture map is the navigation source; ADR-0002 remains the combo
  product decision.
