# ADR-0004: Event streams remain separate projections

**Status:** Accepted  
**Date:** 2026-11-13

## Context

Phase 1 of ADR-0003 proposed a unified `VenueEvent` envelope. Investigation
showed that private and public events have independent sequence domains and
different delivery paths:

```text
private: per-user sequence -> gateway replay/downlink
public:  venue sequence    -> market data, repair, result logs
```

`PrivateEffect` already is the canonical private envelope (core or combo).
`PublicEvent` already is the canonical public envelope (core or combo trade).
`CoreEvent` and `ComboEvent` are internal matching results, not delivery events.

A universal `VenueEvent` would either merge independent sequences or add a
third representation without removing either existing one.

## Decision

Keep one canonical envelope per stream:

```text
PrivateEffect  - private delivery envelope
PublicEvent    - public delivery envelope
```

Do not introduce a universal `VenueEvent` struct. Internal `CoreEvent` and
`ComboEvent` remain matching-layer results and are projected by the runtime.

## Consequences

- Phase 1 closes with domain ids/sequences, not a new event wrapper.
- The architectural rule is “one event truth per stream”, not “one struct for
  all events”.
- Any future event work must update the stream's canonical envelope, not add a
  parallel type.
