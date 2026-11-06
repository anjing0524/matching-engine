# Exchange production configuration

## Runtime defaults

| Setting | Default | Meaning |
|---|---|---|
| `private_replay_capacity` | 1024 | private events retained per user for reconnect |
| `repair_buffer_capacity` | 100000 | public events retained for NACK repair |
| `idle_timeout` | 30s | gateway session timeout |
| `multicast_group` | none | public market-data group |
| `multicast_port` | none | public market-data UDP port |
| `multicast_interface` | none (loopback in tests) | primary NIC used for outgoing multicast |
| `multicast_interfaces` | empty | fallback NICs tried in order if the primary fails |

## Example

```rust
let config = ExchangeConfig {
    private_replay_capacity: 4096,
    repair_buffer_capacity: 1_000_000,
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
cargo run --example exchange_production_config
```

## Deployment notes

- `gateway`: owns member TCP sessions, authentication, seat mapping, rate limits.
- `middleware`: owns global sequencing, command/result log, private routing, public multicast/NACK, phase admission.
- `core`: single authoritative business state machine; no sockets, no wall clock, no file I/O.
- `gateway` and `middleware` may be multi-active; `core` is a single failure domain.
- A multicast address must be routable on the selected interface; selecting `127.0.0.1` is for local tests only.
- `idle_timeout = None` disables gateway expiry.
- Replay and repair capacity should exceed the longest expected reconnect/gap window.
