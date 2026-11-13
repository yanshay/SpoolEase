base_target="spoolease-bin"
VERSION="${1:-0.8}"
path_in_base_target="/bins/${VERSION}"
rel_train="oneoff"
product="console"

source ./deploy-vars.sh || { return 1 2>/dev/null || exit 1; }

pushd "$xtask_dir" || { return 1 2>/dev/null || exit 1; }

cargo xtask ota build --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/${rel_train}" || { popd; return 1 2>/dev/null || exit 1; }
# cargo xtask web-install build --layout updater-ota --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/${rel_train}" --support-dir "$base_target_dir/bins/0.8/console/web-install" --updater-web-install "$base_target_dir/bins/0.8/updater/web-install"
popd
