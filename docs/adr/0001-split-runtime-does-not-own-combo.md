# ADR-0001: Split runtime does not own combo execution

**Status:** Accepted  
**Date:** 2026-11-11

## Context

The single-process `TradingRuntime` composes a `ComboRuntime` beside the core.
The split deployment (`trading-middleware` + `trading-core`) has command,
public-event, and private-event rings for the core only. It has no combo
command ring and no combo private-event producer.

Spec #20 assumed that core and combo replay both exist in both deployment
modes. That assumption was wrong for the split deployment. The shared gateway
effect operation can carry both replay kinds, but the split driver has no combo
source to feed it.

## Decision

Combo execution and combo private replay are owned by the single-process
runtime. The split deployment does not run combo execution and does not promise
combo replay.

If split combo execution becomes a product requirement, it needs its own spec
and its own combo command/private-event transport. It must not be inferred from
this ADR's absence. See ADR-0002 for the exchange-level combo product decision and
the single-process delivery phase.

## Consequences

- Gateway effect application still supports `ReplayEvent::Core` and
  `ReplayEvent::Combo`; the shared seam is ready for a future combo source.
- Ticket #21 is partial: split redirect and shared effect coverage exist;
  split combo replay does not.
- Architecture reviews must not assume split combo support without a new spec.
