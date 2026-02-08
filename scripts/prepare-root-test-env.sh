#!/usr/bin/env bash
# Prepare mounted btrfs test environment for root-required tests.
#
# Output:
#   - state file for cleanup
#   - env file to source before running tests

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
source "$SCRIPT_DIR/test-root-env/lib/common.sh"

ensure_root_access
prepare_workspace "$PROJECT_DIR"
setup_filesystems
setup_luks_if_available
write_state_file
write_env_file

echo "==> Environment prepared"
echo "    Work dir:   $WORK_DIR"
echo "    State file: $STATE_FILE"
echo "    Env file:   $ENV_FILE"
echo
echo "Next steps:"
echo "  source $ENV_FILE"
echo "  bash scripts/test-root-required.sh"
echo "  bash scripts/cleanup-root-test-env.sh"
