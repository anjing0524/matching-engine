# Combo matching design baseline

Status: design baseline for M1/M2. This is not a claim of full exchange
compatibility.

Inputs:

- [ADR-0001: split runtime does not own combo](adr/0001-split-runtime-does-not-own-combo.md)
- [ADR-0002: combo trading is exchange-level and phased](adr/0002-combo-trading-is-exchange-level-and-phased.md)
- [GLOSSARY.md](../GLOSSARY.md)

## Public-rule baseline

The design follows the SHFE-style arbitrage instruction model:

- cross-term: same product, different months
- cross-product: different products, same month
- two legs, equal quantity, opposite direction (1:1)
- spread quote may be negative
- arbitrage orders only in continuous trading, not call auction
- FOK/FAK attach to arbitrage orders
- direct, implied-in, and implied-out execution paths
- leg-level margin; order/cancel frozen at prior settlement, fill at trade price,
  included in single-side largest
- no combo position; positions live on leg Contracts
- implied orders are not published; public book shows base orders only

References:

- [Shanghai Futures Exchange trading-instruction Q&A](https://www.163.com/dy/article/L038UT6B05568W0A.html)
- [SHFE trading-rules revision notice](https://www.shfe.com.cn/publicnotice/notice/202604/t20260428_831546.html)
- [DCE arbitrage Q&A](http://www.dce.com.cn/dalianshangpin/ywfw/ywzy/jyywzy/498201/1500377/index.html)
- [CME implied-price overview](http://www.cme.com/files/ImpliedPriceOverview.pdf)

## Phase A: M1

M1 is a vertical slice, not a full exchange clone:

```text
multi-contract core
  ├── ordinary Contract books
  ├── combo book per Combo Pair
  ├── direct combo ↔ combo
  └── implied-in: two leg orders → implied combo → match
```

M1 supports:

- static pair registry loaded at startup
- spread limit GFD and cancel
- direct combo matching
- implied-in
- FOK/FAK supported through IOC-All/IOC-Any; Market combo supported at resting spread; AdvancedMarket/GIS rejected
- leg-level margin and single-side largest
- leg fills and combo fills
- combo price statistics and leg public trades
- snapshot of pair registry, combo book, leg books/positions/margin, private seq
- reconnect replay of core and combo private events

M1 does not support:

- implied-out
- FOK/FAK
- market arbitrage
- runtime pair creation
- dynamic margin reload
- combo order-book depth market data

## Phase B: M2

M2 adds:

- implied-out: combo + leg → other leg
- FOK/FAK
- AdvancedMarket protection-price behavior is supported: market matching is bounded by the protection spread
- more complete exchange priority and cascade rules
- margin/reload operations

The M1 data model reserves slots for these; M1 must not encode assumptions that
make them impossible.

## Data model

```text
InstrumentRegistry
  ordinary: InstrumentId -> ContractBook + ContractSpec
  pairs:    ComboId -> ComboPair { leg1, leg2, ratio=1, spread tick, margin }

BookSide
  price level -> FIFO base orders

ComboBook
  ComboId -> price levels of resting base ComboOrders
  no stored implied orders

Order
  combo order: ComboOrderId, user/session, side, spread limit, qty, remaining
  leg order:   LegOrderId, ContractId, price, qty, remaining, owner
```

Derived orders are transient matching candidates. They are never inserted into
the public book and never receive public market-data identities.

## Matching trigger and order

All matching runs on the single core thread. One event may trigger several
rounds; mutation is committed only after an atomic plan is valid.

M1 event order:

```text
on leg order / cancel / combo order / cancel:
  1. run ordinary continuous matching on the affected leg book
  2. run direct combo matching for the affected pair
  3. compute implied-in candidates from leg books
  4. re-evaluate resting combo orders as takers against leg liquidity
  5. commit atomic fills, then emit leg and combo effects
```

In M1, a resting combo may execute when leg liquidity appears as a combo taker
against resting legs. Leg aggressors do not receive implied-out fills; that is
M2.

M2 extends step 4 to implied-out:

```text
combo + one leg order -> derived other-leg order -> match against leg book
```

## Direct combo execution price

Two opposite base Combo Orders execute at the SHFE-style direct trade price:
the median of the buy limit, the sell limit, and the last combo trade price on
that pair. Before the first trade, use the midpoint of the two limits, rounded
toward the resting order's limit (down for a buy taker, up for a sell taker).
Every direct fill updates the pair's last combo trade price.

## Atomic fill plan

For each candidate:

```text
plan_buy_combo:
  need = combo.remaining
  walk leg1 ask levels and leg2 bid levels within spread limit
  available = min over both legs and price constraints
  fill = min(need, available)
  if fill == 0 -> no mutation
  M1 GFD: consume `fill`, leave the remainder resting; no single-leg fill
```

M1 permits partial combo fills over time: each fill consumes the same quantity
from both legs, and the remainder stays resting. A single-leg fill is never
produced. IOC-All (FOK) is all-or-none transactionally; IOC-Any (FAK) leaves no residual.

On commit:

1. remove/decrement both leg orders at their own prices
2. update leg positions and frozen margin for leg owners, and apply the combo
   owner's position on both leg Contracts
3. emit ordinary leg fills
4. emit one combo fill to the combo owner
5. update combo price statistics
6. update public trade/volume/open-interest data for legs; combo has price
   statistics but no position

## Priority

M1 priority follows the public base-order rule:

```text
price priority
same price: direct base order before implied
same price, multiple implied: cross-term before cross-product
same price, same product: exchange-defined contract order
time priority within the same class
```

M1 supports only cross-term and same-month cross-product pairs. Cross-product
pairs whose tick sizes do not satisfy the exchange rule are rejected at config
load.

## Margin and risk

M1 does not calculate a synthetic pair margin. It freezes margin on each leg
Contract:

- at combo entry: leg notional using prior settlement and configured margin rate
- at cancel: release the frozen amount
- at fill: replace frozen estimate with actual trade-price-based margin
- apply single-side-largest at the account level for each product/leg
- combo has no position; positions and margin remain on leg Contracts
- if either leg cannot reserve margin, the combo order is rejected atomically

Prior settlement is venue state configured on the registry before matching; it
is not a wire command. A leg without prior settlement freezes zero estimated
margin, while actual trade-price margin still applies on fill.

The private Combo Fill reports both leg prices. An implied-in fill reports the
resting leg prices it consumed. A direct Combo Fill consumes no leg liquidity,
so it reports the best crossing reference price on each leg, falling back to
prior settlement and then zero; its spread remains the direct execution spread.

Required margin for one account and leg is `max(long, short)` of the actual
trade-price margin, never their sum. Frozen estimates are reported separately
and are released as fills consume the order.

Configuration supplies margin rates and limits; M1 does not auto-reload them at
runtime.

## Events and reconnect

- leg owner: ordinary `PrivateEvent` for its leg fill
- combo owner: `ComboPrivateFill` with spread, leg prices, quantity
- combo private fills carry a registry-owned private sequence; reconnect replay
  must merge it with core per-contract sequences (#38)
- snapshot stores combo orders, leg books/positions, margin reservations, and
  sequence state
- reconnect replays private events after `last_private_seq`; combo effects are
  replayed only when a combo source exists

## Runtime wiring (M1)

The single-process production binary drives one `InstrumentRegistry` through
`RegistryRunner`:

- all commands enter the same middleware command ring; `combo_id != 0` is routed
  to `InstrumentRegistry::execute_combo`, everything else to the ordinary
  Contract core named by `instrument_id`
- combo commands never create a second command ring or a second set of leg
  books; the legacy `ComboEngine`/`ComboRuntime` path is not constructed by the
  single-process binary
- startup loads static pairs, prior settlements, and explicit account margin
  balances from `ComboVenueConfig`; a pair config without balances is rejected
- core private events and Combo Private Fills share one gateway per-user private
  sequence, so reconnect replay is ordered across both sources
- `TradingRuntime::snapshot()` stores `RegistrySnapshot`; restore is rejected
  when the snapshot shape does not match the runtime command path
- split deployment remains single-core and does not own combo execution
  (ADR-0001); `CoreRunner` is retained for that path and for tests

## Market data

M1:

- public leg trades update leg price/volume/open-interest
- every combo trade publishes one `ComboTrade` price statistic carrying the
  pair id, realized spread, quantity, and both leg reference prices
- ComboTrade shares the venue public sequence with ordinary Contract events, so
  a publisher or NACK tracker sees one monotonic stream
- no implied orders in public book or public events
- no combo order-book depth publication
- no combo position

## Validation

M1 tests must cover:

- cross-term and same-month cross-product pair config
- explicit product/month metadata on every pair leg, with cross-term requiring
  the same product and different months and cross-product requiring different
  products in the same month
- direct combo match
- implied-in from two leg orders
- atomic all-or-none failure when one leg is short
- leg fills to both leg owners and combo fill to combo owner
- base-order priority over implied at the same price
- a resting combo order triggered by later ordinary leg liquidity
- margin freeze/release/actual and single-side largest
- snapshot/reconnect replay
- unsupported GIS orders rejected; FOK/FAK/Market/AdvancedMarket behavior covered
- no implied orders in public market data

Performance is measured after correctness; the current 500k/s ingress baseline is
not a combo matching target.
