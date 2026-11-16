# Coding standards

The review agent reads this file. Mechanical rules belong in CI; judgement calls
belong here. Contribution process and commit workflow remain in
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Rust

- Follow the [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html).
- `cargo fmt --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` are mandatory.
- Import type names and use them unqualified; do not spell `std::fmt::Display` inline when an import is available.
- Public structs with public fields are `#[non_exhaustive]`.
- Public API has doc comments, including public fields and error modes.
- `#[must_use]` on functions whose return value carries state or events.
- Prefer a named struct over a tuple for domain values with more than one field.

## Errors

- Use `Result` with a named error type when a caller can distinguish failure modes.
- Do not flatten a typed error into `String` unless the outer interface already requires it.
- Do not add a test-only accessor to production API; keep it under `#[cfg(test)]` or test the real seam.

## Tests

- Test external behaviour at the highest existing seam.
- Prefer a real protocol/process test over a mock when the existing seam already exists.
- A test that only reaches into private fields is a signal the seam is wrong.
- Bug fixes include a regression test that fails before the fix.

## Commits

Conventional Commits are enforced by CI:

```text
feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert
```

Keep merge commits out of the mechanical check.

## Performance

- CPU affinity is mandatory; see `docs/agents/test-environment.md`.
- Build release binaries before measuring.
- Record median of at least three runs; do not record a best-of-one.
- Unpinned, stale-process, or WiFi-client measurements are invalid.

## Documentation

- Removing or renaming a public interface requires updating all docs in the same change.
- Record architecture assumptions as ADRs under `docs/adr/`.

## Async tests

Tests for logs, sockets, child processes, and runtime fanout must wait for a
condition with a bounded deadline. Do not `sleep` for a fixed time and then
assert. Use a `wait_until`/poll helper and make the failure message report what
never arrived.
