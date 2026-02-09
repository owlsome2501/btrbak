#!/usr/bin/env bash
# Compare human-readable and machine-readable snapper list output.

set -euo pipefail

config="${1:-root}"
description="${2:-}"
columns="number,description,type"

echo "== Default table output (human-readable) =="
snapper -c "$config" list --columns "$columns" | sed -n '1,12p'

echo
echo "== TSV output (machine-readable; recommended for scripts) =="
snapper --csvout --separator $'\t' --no-headers -c "$config" list --columns "$columns" \
    | sed -n '1,12p' | cat -vet

if [[ -n "$description" ]]; then
    echo
    echo "== Snapshot IDs for description='$description' and type='single' =="
    snapper --csvout --separator $'\t' --no-headers -c "$config" list --columns "$columns" \
        | awk -F $'\t' -v d="$description" '$2 == d && $3 == "single" { print $1 }' \
        | sort -nr
fi

cat <<'EOF'
Tip: use this format in automation:
  snapper --csvout --separator $'\t' --no-headers -c <config> list --columns number,description,type
EOF
