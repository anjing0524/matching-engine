# Glossary

## Contract

A single tradable futures instrument with its own order book, tick size, price
limits, and expiry. Ordinary orders and implied combo legs trade against the
same Contract order book.

## Leg

One side of a Combo Pair. A Leg is a real Contract, not a private book owned by
the combo engine.

## Combo Pair

An exchange-defined two-leg spread contract. For the first version it follows
the SHFE-style rules:

```text
leg1 and leg2
quantity ratio 1:1
cross-term: same product, different months
cross-product: different products, same month
```

A Combo Pair has an identifier used by Combo Orders and is present in the
instrument registry at startup.

## Leg Contract

The ordinary Contract underlying one Leg of a Combo Pair. Leg Contracts have
their own order books, positions, margin, and market data.

## Spread Price

The quoted price of a Combo Order. It may be negative.

```text
buy spread  = leg1 bid - leg2 ask
sell spread = leg1 ask - leg2 bid
```

For cross-term spreads leg1 is the near month and leg2 the far month. For
cross-product spreads leg1 and leg2 are the two products in exchange-defined
order.

The Combo Pair tick follows the exchange rule: cross-term uses the leg tick;
cross-product uses the common tick rule for the two products.

## Combo Order

An order for a Combo Pair. Supports limit GFD, Market, IOC-Any (FAK),
IOC-All (FOK), Market, AdvancedMarket (market with protection price), and
cancel. GIS orders are deferred. Combo orders do not participate in call auction.
A Combo Order may fill partially over time, but every fill is atomic: both legs
fill the same quantity together. A single-leg fill is never produced.

## Combo Fill

One atomic execution of a quantity from a Combo Order. Both legs fill the same
quantity together at the resting leg prices; the combo owner receives a Combo
Fill report. A Combo Order may have several Combo Fills. The Combo
Pair itself has no position; positions live on the leg Contracts.

## Leg Fill

The ordinary fill produced for the owner of a resting leg order consumed by a
Combo Fill. It is the same Contract-level fill that an ordinary trade produces.

## Implied-in

Two leg orders combine into an implied Combo Order and execute against a
resting or incoming Combo Order.

## Implied-out

A Combo Order combines with an opposite leg order to produce the other leg
order, which then executes against a leg order. Implied-out is first-class
exchange behavior, not a later extension.

## Direct Combo Matching

Two opposite Combo Orders on the same Combo Pair match directly.

## Base Order Priority

At the same price, directly entered leg orders take priority over implied
orders. Among implied orders at the same price, cross-term precedes
cross-product, and the exchange-defined contract order breaks ties.
