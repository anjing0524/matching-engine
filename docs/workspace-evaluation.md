# Crate split evaluation

Date: 2026-11-10

## Question

Should the project be split into a Cargo workspace with multiple crates?

## Current facts

```text
Rust source:        18,313 lines
Source files:       60
Cargo package:      trading-system-engine (one package)
Binaries:           trading-engine
                    trading-core
                    trading-middleware
                    trading-gateway
Direct dependencies: 9
Clean release build: 52.07s on the Fedora i7-8559U (8 logical CPUs)
```

Current grouped modules:

```text
trading/
├── core/          deterministic state
├── gateway/       member sessions
├── transport/     rings, sequencing, logs
├── market_data/   publisher, tracker, repair, metrics
├── combo/         queue, engine, exec, runtime, margin
├── snapshot/      snapshot file, replay
└── runtime/       composition, event fanout
```

## Internal dependency cycles

A naive workspace split is blocked by these module cycles:

```text
core      -> snapshot
snapshot  -> core

gateway   -> transport
transport -> gateway

combo     -> gateway
```

- `snapshot::replay` needs `TradingCore`; `core` snapshot methods need
  `snapshot_file`.
- `transport::middleware` owns `SessionRegistry`; `gateway::runtime` uses
  `MiddlewareSequencer`.
- `combo::runtime` dispatches through `GatewayRuntime`; gateway exposes combo
  private fills.

Crates cannot contain cyclic dependencies, so these seams must be moved or
inverted first.

## Rust convention

Rust does not require a workspace. A package with multiple `src/bin` binaries
and grouped modules is idiomatic, especially when:

- there is one deployable system;
- there is one external API;
- the binaries share the same internal types;
- compile times are already acceptable;
- no component is consumed independently.

A workspace is justified when at least one is true:

- independent libraries are published or reused outside the system;
- different teams own different crates;
- clean/rebuild time becomes a demonstrated bottleneck;
- bins need genuinely different dependency/feature sets;
- a stable public interface must be enforced between components.

None of those is currently true.

## Recommendation

**Do not split the package now.** Keep one crate with the grouped module layout.

Reasons:

1. A clean release build is ~52s; modular compilation is not a pain point.
2. One command path, one deployment, one release; splitting adds Cargo
   boilerplate without product separation.
3. The current cycles are better fixed as module seams first. Moving them just
   to satisfy Cargo would worsen the architecture.
4. Separate crates would force internal gateway/session/combo types into public
   interfaces prematurely.

## If and when to split

Prefer a two-step path, not a five-crate explosion.

### Step 1: break cycles as module seams

- Move `snapshot_file` to kernel-owned persistence; move `replay` to the
  runtime-level replay module.
- Move `SessionRegistry`/`SessionInfo` into transport middleware, or define a
  session port owned by transport.
- Move `ComboRuntime` out of the combo module into runtime composition, leaving
  combo with queue/engine/exec/margin only.

### Step 2: split into three crates

```text
trading-domain     order book, SymbolId, order protocol types
trading-kernel     core/kernel, auction, positions, plugins, events,
                   combo queue/engine/exec/margin, core snapshot
trading-runtime    gateway, transport, market_data, runtime, bins
```

Dependency direction:

```text
trading-runtime -> trading-kernel -> trading-domain
```

Only do this when an external consumer, team boundary, or measured compile-time
need justifies the extra public-interface work.

## Decision

Keep `trading-system-engine` as one Cargo package. Revisit after the module
cycles above are removed, or when a concrete reuse/build-time requirement
appears.
