#!/usr/bin/env bash
# Enable the repository's versioned Git hooks.
set -euo pipefail

cd "$(dirname "$0")/.."
git config core.hooksPath .githooks
echo "Git hooks enabled (gitleaks runs on every commit)."
