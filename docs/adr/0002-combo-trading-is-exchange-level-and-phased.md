# ADR-0002: Combo trading is exchange-level and phased

**Status:** Accepted  
**Date:** 2026-11-11

## Context

Cross-period and cross-product arbitrage are core futures business. The repo
already has a two-leg combo engine, but the production binaries do not wire it,
and the current engine owns separate leg books rather than sharing ordinary
Contract books.

Public exchange rules provide the required semantics. The SHFE trading-instruction
Q&A describes exchange-listed arbitrage contracts, direct combo matching,
implied-in and implied-out, leg-level margin with single-side largest calculation,
and market data where implied orders are not published. See the citations in
`docs/combo-matching-design.md`.

Implementing direct, implied-in, implied-out, all order types, and the full
margin model in one delivery is too large and would obscure the core matching
correctness.

## Decision

1. Combo trading is an exchange-level product, not a member-side strategy.
2. The matching core is single-threaded and holds multiple Contract books plus
   combo books in one instrument registry.
3. Legs are real Contracts with shared ordinary liquidity; a Combo Pair has
   1:1 quantity and no position of its own.
4. Delivery is phased:
   - M1: pair registry, multi-contract core, combo book, direct combo matching,
     implied-in, limit GFD + cancel, leg-level margin, combo/leg market data,
     snapshot/reconnect.
   - M2: implied-out, FOK/FAK, market orders, the remaining exchange priority
     rules, and richer margin/reload behavior.
5. The M1 data model must not preclude implied-out; derived orders are not
   stored in the public book and must not appear in market data.
6. M1 rejects unsupported order types explicitly instead of silently treating
   them as GFD.

## Consequences

- Single-process production wiring becomes the first delivery target; split
  combo remains out of scope per ADR-0001.
- The existing standalone `ComboExecutor` books are replaced by access to the
  shared Contract books; they must not become a second source of liquidity.
- M1 is not a complete exchange-compatible arbitrage engine. It is a vertical
  slice with direct and implied-in liquidity.
- Tests must distinguish base-order priority, combo priority, atomic fill, and
  leg/combo fill reporting.
