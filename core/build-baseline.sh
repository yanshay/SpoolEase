#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$script_dir/../../deploy-config.sh"
echo "The console and esp-hal-app-updater must both be built before running this script."

pushd "$xtask_dir"
cargo xtask ota build --input "$script_dir"
cargo xtask ota build --input "$xtask_dir/esp-hal-app-updater"
popd
