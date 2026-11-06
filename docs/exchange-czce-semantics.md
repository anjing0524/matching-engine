# CZCE FOK/FAK semantics check

## External references

- Secondary market note stating all six Chinese futures exchanges support FOK/FAK
  and that FOK/FAK must not be used during call auction:
  <https://m.sohu.com/coo/zmtq/1046694145_122014422>
- CZCE trading rules PDF (official, not automatically text-extracted here):
  <https://www.czce.com.cn/cn/uploadfile/2021/11/29/20211129113632572.pdf>

## Current implementation

- FOK: `volume_condition == All`
  - pre-checks aggregate crossing liquidity
  - rejects before matching when the full quantity cannot fill
  - no partial fill is published
- FAK: `time_condition == Ioc` with `volume_condition == Any`
  - fills crossing liquidity
  - cancels the unmatched remainder
- Market orders sweep the book; IOC remainder is canceled.
- Opening auction now rejects market, IOC/FAK, and FOK commands with
  `auction rejects market/FAK/FOK orders`.
- Limit + GFD remains the auction-compatible order shape in this core.

## Remaining validation

The secondary source confirms the general rule, but the official CZCE PDF has
not been machine-verified in this repository. Before production, confirm:

1. whether CZCE emits a rejected order or an accepted-then-canceled order when
   FOK cannot fill
2. exact auction order-type restrictions
3. any product-specific exceptions

Until then, readiness keeps official CZCE verification open.
