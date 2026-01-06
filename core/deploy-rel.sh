base_target="spoolease-bin"
path_in_base_target="/bins/0.6"
product="console"

source ./deploy-vars.sh

pushd ../../esp-hal-app
pushd ${xtask_dir} 
cargo xtask ota build --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/ota"
cargo xtask web-install build --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/web-install"
