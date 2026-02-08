#!/usr/bin/env bash
# Cleanup mounted btrfs test environment created by prepare-root-test-env.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/test-root-env/lib/common.sh"

STATE="${1:-${BTRBAK_TEST_ENV_STATE_FILE:-}}"
if [[ -z "$STATE" ]]; then
    echo "ERROR: missing state file path." >&2
    echo "Usage: bash scripts/cleanup-root-test-env.sh <state-file>" >&2
    echo "or set BTRBAK_TEST_ENV_STATE_FILE from env.sh" >&2
    exit 1
fi

ensure_root_access
load_state_file "$STATE"
echo "==> Cleaning up test environment at $WORK_DIR"
cleanup_from_state
echo "==> Cleanup finished"
