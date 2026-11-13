base_target="spoolease-bin"
path_in_base_target="/bins/0.8"
product="console"

source ./deploy-vars.sh || { return 1 2>/dev/null || exit 1; }

pushd "$xtask_dir" || { return 1 2>/dev/null || exit 1; }
cargo xtask ota build --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/ota" || { popd; return 1 2>/dev/null || exit 1; }
cargo xtask web-install build --layout updater-ota --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/web-install" --support-dir "$base_target_dir${path_in_base_target}/${product}/web-install" --updater-web-install "$base_target_dir${path_in_base_target}/updater/web-install" || { popd; return 1 2>/dev/null || exit 1; }
popd
