#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace_dir=$(cd -- "$script_dir/../../.." && pwd)
xtask_dir="$workspace_dir/new-ota-esp-hal-app"
output_dir="$workspace_dir/spoolease-bin/bins/0.8/console/ota"

mkdir -p "$output_dir"

pushd "$xtask_dir"
cargo xtask ota build --input "$script_dir" --output "$output_dir"
popd
