#!/usr/bin/env bash
# Validate Conventional Commits subjects in a git range.
#
# Usage:
#   scripts/check-commit-messages.sh <base>..<head>
#   scripts/check-commit-messages.sh --last N
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "${1:-}" == "--last" ]]; then
  range="HEAD~${2:-1}..HEAD"
elif [[ $# -eq 1 ]]; then
  range=$1
else
  echo "usage: $0 <base>..<head> | --last N" >&2
  exit 2
fi

pattern='^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\([a-z0-9._/-]+\))?!?: .+'
failed=0
while IFS= read -r subject; do
  if [[ ! "$subject" =~ $pattern ]]; then
    echo "invalid commit subject: $subject" >&2
    failed=1
  fi
done < <(git log --no-merges --format=%s "$range")
if (( failed )); then
  echo "commit messages must follow Conventional Commits" >&2
  exit 1
fi
echo "commit messages ok: $range"
