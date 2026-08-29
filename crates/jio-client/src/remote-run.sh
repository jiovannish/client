#!/usr/bin/env bash
set -euo pipefail

readonly core=/home/ubuntu/jio-core/target/release/jio-core
readonly catalog=/home/ubuntu/.local/share/jio/templates
readonly workload=$1
readonly instances=$2

cleanup() {
    local run_code=$?
    trap - EXIT
    rm -f -- "$workload"
    exit "$run_code"
}
trap cleanup EXIT

[[ -x "$workload" ]]
[[ -x "$core" ]]
[[ "$instances" =~ ^[1-9][0-9]*$ ]]

templates=()
while IFS= read -r -d '' manifest; do
    if jq -e '.format == "jio-runtime-template-v0"' "$manifest" >/dev/null; then
        templates+=("$(dirname "$manifest")")
    fi
done < <(find "$catalog" -mindepth 2 -maxdepth 2 -name manifest.json -print0)

if ((${#templates[@]} != 1)); then
    printf 'expected exactly one runtime template, found %s\n' "${#templates[@]}" >&2
    exit 1
fi

readonly template=${templates[0]}
"$core" "$template/template.json" "$template/memory" "$workload" --instances "$instances"
