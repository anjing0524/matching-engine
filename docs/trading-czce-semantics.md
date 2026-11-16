# CZCE FOK/FAK semantics check

## External references

- Secondary market note stating all six Chinese futures tradings support FOK/FAK
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

## Official rules text (extracted 2026-11-06)

From the official CZCE trading rules PDF, 第二十六条:

- order types listed: 限价指令、市价指令、套利指令、取消指令
- 市价指令不参与集合竞价
- 套利指令不参与集合竞价
- single-side no-quote: unfilled market order is automatically canceled
- minimum order quantity is 1 lot
- limit order maximum is 1000 lots; market order maximum is 200 lots
- FOK/FAK are **not** listed as order types in this 2021 text

This matches the core's auction rejection for market orders. FOK/FAK are treated
as order attributes by brokers/trading systems, not as CZCE order types in the
2021 rules. The secondary source says all six tradings support these attributes
and that they are not allowed in auction; product-level confirmation is still
needed.

## Remaining validation

Before production, confirm:

1. whether CZCE emits a rejected order or an accepted-then-canceled order when
   FOK cannot fill
2. product-level availability of FOK/FAK attributes
3. any product-specific exceptions
