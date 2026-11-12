base_target="spoolease-bin"
VERSION="${1:-0.8}"
path_in_base_target="/bins/${VERSION}"
rel_train="oneoff"
product="console"

source ./deploy-vars.sh

pushd ${xtask_dir}

cargo xtask ota build --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/${rel_train}"
# cargo xtask web-install build --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/${rel_train}"
popd
