# Trading failure modes and upgrade policy

## Disk-full / log failure

- Command/result log append errors are returned to the caller; middleware fails
  fast instead of silently dropping audit data.
- Snapshot writes go to a temporary file and are renamed only after a successful
  write. A failed write leaves the previous snapshot untouched.
- If no writable log location exists, do not start the process.

## Shared-memory / mmap failure

- Ring create/open failures abort startup.
- A corrupted slot (length beyond slot size) advances the consumer and is
  surfaced to the caller; it is never copied out of bounds.
- Mismatched ring slot sizes are rejected at open time.
- Core and middleware must be restarted together if the ring layout changes.

## Snapshot failure

- Invalid magic/version/CRC32 rejects the snapshot.
- Restore failure is fail-closed; fall back to an older snapshot plus full
  command-log replay.
- Never overwrite the only snapshot; keep at least two generations.

## Public market-data failure

- Consumers detect sequence gaps and send NACKs.
- The repair server has a bounded replay buffer and NACK rate limit.
- If repair cannot satisfy a gap, the consumer should reconnect/rebuild; there
  is no full book snapshot stream.

## Rolling upgrade policy

The wire/frame/command layouts are currently versioned only inside snapshots and
logs, not negotiated between gateway and middleware. Until protocol negotiation
exists:

1. stop the gateway listener, close its connections, and stop new logins
2. stop core after the command log is flushed
3. upgrade middleware and core together with matching ring layouts
4. restore the core snapshot and replay the post-snapshot command log
5. restart gateway listeners and let clients reconnect with `last_private_seq`

Do not run mixed binary versions against the same rings or frame protocol.

## Audit retention policy

- Command and result logs are size-bounded and rotated.
- Retain enough generations to cover the longest replay/reconnect window.
- Archive removed segments out-of-band; do not rely on a single host for audit
  retention.
- Snapshot cadence must be more frequent than command-log retention.
