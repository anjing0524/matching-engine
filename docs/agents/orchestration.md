# Agent orchestration notes

## Implementer limit

This environment allows at most two active child agents. Run at most two
implementers at a time; do not launch a third and assume it was queued.

## Worktree contract

Each implementer works in its own worktree under `.worktrees/` and commits its
own branch. Before reporting, verify:

```bash
git -C .worktrees/t<N> status --short
git -C .worktrees/t<N> log --oneline -3
```

A silent child failure leaves no closing message. If a worktree has no commit
or only partial edits, the Lead takes over; do not wait indefinitely.

## Merge contract

The Lead merges branches one at a time into the integration branch and reruns
the full suite after each merge. Do not merge two overlapping worktrees
simultaneously.

## Context pointers

Use issues #20-#26 and `docs/architecture-map.md` as pointers. Do not duplicate
the spec into subagent prompts.

## Cleanup

Remove worktrees and ticket branches after merging:

```bash
git worktree remove --force .worktrees/t<N>
git branch -D ticket/<N>
```
