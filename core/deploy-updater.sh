#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace_dir=$(cd -- "$script_dir/../../.." && pwd)
source "$script_dir/../../deploy-config.sh"
updater_dir="$xtask_dir/esp-hal-app-updater"
output_dir="$workspace_dir/spoolease-bin/bins/0.8/updater/ota"
web_output_dir="$workspace_dir/spoolease-bin/bins/0.8/updater/web-install"

mkdir -p "$output_dir" "$web_output_dir"

pushd "$xtask_dir"
cargo xtask ota build --input "$updater_dir" --output "$output_dir"
cargo xtask updater-web-install build --input "$updater_dir" --output "$web_output_dir"
popd
