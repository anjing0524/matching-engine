## Rust style

Rust code must follow the [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html). Adherence is mandatory: apply it to every `.rs` file you create or modify.

## Agent skills

### Issue tracker

Issues live in GitHub Issues for `anjing0524/matching-engine`, managed with `gh`. See `docs/agents/issue-tracker.md`.

### Triage labels

Default canonical labels: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Review standards

Review work reads `CODING_STANDARDS.md`. Mechanical checks live in CI; judgement
calls live there.

### Test environment

Performance and integration runs read `docs/agents/test-environment.md`
(CPU pinning, preflight, remote host, valid-run criteria).

### Agent orchestration

Parallel implementers read `docs/agents/orchestration.md` (two-child limit,
worktrees, merge/cleanup contract).

### Domain docs

Single-context: root `GLOSSARY.md` plus `docs/adr/`. See `docs/agents/domain.md`.
