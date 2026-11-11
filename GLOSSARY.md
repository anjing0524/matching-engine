# Glossary

## Contract

A single tradable futures instrument with its own order book, tick size, price
limits, and expiry. Ordinary orders and combo legs trade against the same
Contract order book.

## Leg

One side of a Combo Pair. A Leg is a real Contract, not a private book owned by
the combo engine.

## Combo Pair

A static definition of two Contracts that may be traded atomically:

```text
leg1 Contract
leg2 Contract
positive integer ratio
spread tick
margin parameters
```

A Combo Pair has an identifier used by Combo Orders. Pairs are loaded at
startup; runtime pair creation is out of scope for the first version.

## Spread Price

The quoted price of a Combo Order:

```text
spread = leg1 price - ratio * leg2 price
```

Buying the spread means buying leg1 and selling leg2. The spread is quoted in
leg1 price ticks.

## Combo Order

An order for a Combo Pair whose two Legs must fill together. First version:
spread limit, GFD, cancel. No partial fill; no single-leg fill.

## Combo Fill

One atomic execution of a Combo Order. It consumes resting liquidity from both
Leg order books at their own prices and reports the resulting leg prices and
quantity to the combo owner.

## Leg Fill

The ordinary fill produced for the owner of a resting leg order consumed by a
Combo Fill. It is the same Contract-level fill that an ordinary trade produces.

## Implied-in

A Combo Order consuming resting liquidity from its Leg order books.

## Implied-out

An ordinary Leg order interacting with a resting Combo Order. Deferred beyond
the first comma version; the pair model must not preclude it.

## Atomic

Both Legs fill in the same matching operation, or neither fills.
