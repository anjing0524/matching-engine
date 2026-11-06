# Exchange core production readiness checklist

Status reflects the current gateway/middleware/core slices in this repository.

## Correctness

- [x] Deterministic command → matching → event kernel (no wall clock, no sockets)
- [x] Fixed 256-byte command layout and explicit reject paths
- [x] Price/time priority and multi-level matching tests
- [x] Auction uncross with tie-break tests
- [x] FAK/FOK, market orders, IOC remainder cancellation
- [x] Gross positions with today/yesterday and close reservation
- [x] Session phase and time-tick control from middleware
- [x] Combo queue, atomic two-leg execution, open/close positions
- [x] Combo per-user open quantity limit and optional margin precheck
- [ ] Official CZCE verification of FOK semantics
- [ ] Position-limit treatment of pending open orders (exchange-specific细则)
- [ ] Combo close reservation split by yesterday/today reason codes

## Gateway / sessions

- [x] Fixed 264-byte gateway frame (login/command/heartbeat/ack/private/combo-private)
- [x] TCP accept loop and per-connection event stream
- [x] Login opens middleware session; commands are session/member stamped
- [x] Idle timeout expires sessions and closes TCP
- [x] Client private ack updates `last_private_seq`
- [x] Reconnect replays private events after ack
- [x] Shared login token authentication
- [x] Per-member token credentials
- [x] Seat permissions from the per-member credential registry
- [x] Per-connection command rate limiting
- [x] Same member/user/seat login takes over the previous session
- [x] Planned gateway session drain API
- [ ] Automated cross-gateway failover and client redirection
- [x] Split core/middleware binaries over shared-memory rings
- [x] Standalone gateway TCP relay to middleware

## Middleware / transport

- [x] Monotonic global sequence assignment and command ring
- [x] Combo commands routed to a separate command ring
- [x] Shared-memory command/public/private rings with fixed slots
- [x] Allocation-free slot pop into stack buffers
- [x] Public seq gap detection and NACK request from gateway
- [x] NACK-driven repair replay buffer
- [x] Public UDP unicast/multicast publisher and subscriber
- [x] Append-only command-log segments with header/version checks
- [ ] Automated segment rotation/retention
- [ ] Persistent full-day public/private result log
- [ ] NACK rate limiting and anti-amplification policy
- [ ] Multicast interface failover

## Failure semantics

- [x] Fail-fast: malformed frames disconnect the gateway connection
- [x] No per-message application ack; private replay uses `last_private_seq`
- [x] Core is a single failure domain; no consensus/leader election
- [x] In-memory core snapshot for continuous book and business state
- [x] Versioned JSON snapshot with atomic file replacement
- [x] CRC32 checksum over snapshot payload
- [x] Replay-from-snapshot helper and deterministic test
- [x] Replay runbook and `exchange-core --snapshot` restore entry point
- [ ] Automated command-log segment management
- [ ] Disk-full / mmap failure behavior for shared-memory and result logs

## Observability

- [x] Deterministic event streams and sequence numbers
- [x] Runtime counters for gateway events, commands, private/public sends,
      repair, replay, and idle expiry
- [x] Prometheus text rendering for those counters
- [x] HTTP `/metrics` scrape endpoint
- [x] Tick latency histogram in Prometheus format
- [ ] Structured audit log retention policy

## Performance

- [x] Core command benchmark (crossing/resting)
- [x] Middleware submit + core consume benchmark
- [x] Combo queue/cancel/atomic-fill benchmark
- [x] 1M command core soak and combo queue/fill soak
- [x] Nightly soak workflow and reproducible soak script
- [x] First manual soak run green (core, combo, TCP/UDP)
- [ ] Multi-hour soak on target hardware
- [ ] End-to-end TCP+multicast load test with many gateways

## Deployment

- [x] Validated `ExchangeConfig` and multicast publisher example
- [x] Production configuration notes
- [x] systemd unit templates and environment example
- [x] Multi-stage Alpine Dockerfile; image built and smoke-run locally
- [ ] Core state snapshot/restore procedure
- [ ] Rolling upgrade and compatibility policy
