# Test environment

## Topology

The current benchmark host is 4 physical cores / 8 hardware threads:

```text
CPU0-3  physical cores
CPU4-7  SMT siblings of CPU0-3
```

CPU affinity is mandatory. Results from unpinned runs are invalid.

## Preflight

```bash
scripts/check-test-host.sh --strict
```

Checks `taskset`, CPU topology, load average, stale trading processes, and
whether release binaries are older than source. Aggregate scripts call it
automatically.

## Pinned single-shard baseline

```bash
MIDDLEWARE_CPUS=0,1 CORE_CPU=2 CLIENT_CPU_BASE=3 CLIENT_CPU_SPAN=5 \
  scripts/run-load-clients.sh 8 15000 32
```

## Remote host

The Fedora host is reached as `ssh fedora`. Prefer git push/fetch over
`git bundle` + `scp`:

```bash
git push origin <branch>
ssh fedora 'cd ~/code/matching-engine && git fetch origin <branch> && git switch -C <branch> FETCH_HEAD'
```

If the host is unreachable, do not invent benchmark numbers. Either wait, or
run functional checks locally with the full Cargo cache; label local runs as
local.

Before a performance run, kill stale test processes by executable path, not by
a broad command-line pattern that can match the shell itself.

## What invalidates a run

- unpinned processes
- stale `trading-*` busy-poll processes
- stale release binaries
- external builds or containers saturating the host
- a client on a WiFi link when measuring server capacity
