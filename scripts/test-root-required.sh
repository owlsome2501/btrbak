#!/usr/bin/env bash
# Run root-required tests only.
#
# This script keeps a minimal responsibility:
# 1) build test executables
# 2) run root_required_tests under sudo/root
# Environment validation is handled by Rust tests themselves.

set -euo pipefail

ROOT_FILTER="root_required_tests"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
SUDO_PASSWORD_FILE="${BTRBAK_SUDO_PASSWORD_FILE:-}"

test_args=()
if [[ "${1:-}" == "--" ]]; then
    shift
    test_args=("$@")
elif [[ "$#" -gt 0 ]]; then
    test_args=("$@")
fi

export BTRBAK_STRICT_INTEGRATION="${BTRBAK_STRICT_INTEGRATION:-1}"

cd "$PROJECT_DIR"
echo "==> Building test executables (no run)..."
build_log="$(mktemp)"
if ! cargo test --no-run --color never "$ROOT_FILTER" >"$build_log" 2>&1; then
    cat "$build_log" >&2
    rm -f "$build_log"
    exit 1
fi
cat "$build_log"

mapfile -t TEST_BINS < <(
    sed -nE 's@^ +Executable .* \((target/debug/deps/[^)]+)\)$@\1@p' "$build_log" | sort -u
)
rm -f "$build_log"

if [[ "${#TEST_BINS[@]}" -eq 0 ]]; then
    echo "ERROR: cargo did not report any test executables for '$ROOT_FILTER'." >&2
    exit 1
fi

run_as_root() {
    if [[ "$(id -u)" -eq 0 ]]; then
        "$@"
    else
        if sudo -n true >/dev/null 2>&1; then
            sudo -E "$@"
            return
        fi

        if [[ -n "$SUDO_PASSWORD_FILE" ]]; then
            if [[ ! -r "$SUDO_PASSWORD_FILE" ]]; then
                echo "ERROR: sudo password file is not readable: $SUDO_PASSWORD_FILE" >&2
                return 1
            fi
            # Password files often end with a newline. Strip CR/LF before feeding sudo.
            local sudo_password
            sudo_password="$(tr -d '\r\n' < "$SUDO_PASSWORD_FILE")"
            if [[ -z "$sudo_password" ]]; then
                echo "ERROR: sudo password file is empty: $SUDO_PASSWORD_FILE" >&2
                return 1
            fi
            printf '%s\n' "$sudo_password" | sudo -S -E -p '' "$@"
            return
        fi

        sudo -E "$@"
    fi
}

echo "==> Running root-required tests as root..."
echo "==> Strict checks: $BTRBAK_STRICT_INTEGRATION"
echo "==> Built test executables: ${#TEST_BINS[@]}"
total_root_tests=0
for bin in "${TEST_BINS[@]}"; do
    if [[ ! -x "$bin" ]]; then
        continue
    fi
    count="$( "$bin" --list "$ROOT_FILTER" 2>/dev/null | sed -n 's/: test$//p' | wc -l | tr -d ' ' )"
    if [[ "$count" =~ ^[0-9]+$ ]]; then
        total_root_tests=$((total_root_tests + count))
    fi
done
echo "==> Discovered root-required tests: $total_root_tests"
failed_bins=()
for bin in "${TEST_BINS[@]}"; do
    if [[ ! -x "$bin" ]]; then
        continue
    fi
    if ! run_as_root chmod a+rx "$bin"; then
        failed_bins+=("$bin (chmod)")
        echo "==> Failed: $bin (chmod)" >&2
        continue
    fi
    if ! run_as_root "$bin" "$ROOT_FILTER" --nocapture --test-threads=1 "${test_args[@]}"; then
        failed_bins+=("$bin")
        echo "==> Failed: $bin ($ROOT_FILTER)" >&2
        continue
    fi
    echo "==> Completed: $bin ($ROOT_FILTER)"
done

if [[ "${#failed_bins[@]}" -gt 0 ]]; then
    echo "ERROR: root-required test run failed in ${#failed_bins[@]} test executable(s):" >&2
    for failed in "${failed_bins[@]}"; do
        echo "  - $failed" >&2
    done
    exit 1
fi
