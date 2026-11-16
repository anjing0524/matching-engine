# Trading production configuration

## Runtime defaults

| Setting | Default | Meaning |
|---|---|---|
| `private_replay_capacity` | 1024 | private events retained per user for reconnect |
| `idle_timeout` | 30s | gateway session timeout |
| `multicast_group` | none | public market-data group |
| `multicast_port` | none | public market-data UDP port |
| `multicast_interface` | none (loopback in tests) | primary NIC used for outgoing multicast |
| `multicast_interfaces` | empty | fallback NICs tried in order if the primary fails |

## Example

```rust
let config = TradingConfig {
    private_replay_capacity: 4096,
    idle_timeout: Some(Duration::from_secs(15)),
    multicast_group: Some("239.10.20.30".to_string()),
    multicast_port: Some(40_000),
    multicast_interface: Some("192.168.10.20".to_string()),
};
config.validate()?;
let publisher = config.multicast_publisher("0.0.0.0:0")?;
```

Run the example:

```text
cargo run --example trading_production_config
```

## Combo venue configuration

The single-process `trading` binary always uses the registry runtime. Combo
trading is enabled by `--combo-config <json>`; without it the registry contains
the default single `BTCUSDT` Contract and any non-zero `combo_id` is rejected as
an unknown pair rather than silently treated as an ordinary order.

`ComboVenueConfig` declares ordinary Contracts, static Combo Pairs, prior
settlements, and explicit account margin balances. See
[`deploy/combo-venue.example.json`](../deploy/combo-venue.example.json).

Every Contract named as a Combo Pair leg must declare `product` and `month`
(`YYMM`) metadata. Startup validation rejects a pair leg without usable
metadata and enforces the pair kind: `cross_term` legs must share a product and
have different months, while `cross_product` legs must have different products
and share a month. Contracts used only by ordinary commands may omit both
fields.

Fail-closed rules:

- a config with Combo Pairs and no margin balances is rejected at startup;
- every account that submits a Combo Order must have an explicit balance;
  an unconfigured account is rejected at entry instead of trading unlimited;
- a registry snapshot restore also enables this rule when the restored venue
  contains pairs, because balances are configuration, not snapshot state.

`--registry-snapshot <path>` restores registry-backed combo state directly.
`--snapshot <path>` restores a full `TradingSnapshot`; the registry shape and
the runtime command path must match. `--snapshot-out <path>` writes the full
runtime snapshot on exit.

## Deployment notes

- `gateway`: owns member TCP sessions, authentication, seat mapping, rate limits.
- `middleware`: owns global sequencing, command/result log, private routing, public multicast/NACK, phase admission.
- `core`: single authoritative business state machine; no sockets, no wall clock, no file I/O.
- `gateway` and `middleware` may be multi-active; `core` is a single failure domain.
- A multicast address must be routable on the selected interface; selecting `127.0.0.1` is for local tests only.
- `idle_timeout = None` disables gateway expiry.
- Replay and repair capacity should exceed the longest expected reconnect/gap window.
