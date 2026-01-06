base_target="spoolease-bin"
path_in_base_target="/bins/0.6"
page="debug.html"
rel_train="debug"

product="console"

source ./deploy-vars.sh

#----

replace_dir=""
case "$product" in
  "console") replace_dir="./deploy-fix-html.sh";;
  "scale") replace_dir="../console/core/deploy-fix-html.sh";;
  *) echo "Not a valid product"; exit 1;;
esac

pushd ${xtask_dir} 
cargo xtask ota build --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/${rel_train}"
cargo xtask web-install build --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/${rel_train}"
popd

replace=$(grep '^version' Cargo.toml | sed -E 's/version *= *"([^"]+)".*/\1/')
${replace_dir} "$base_target_dir${path_in_base_target}/${page}" ${product} "$replace"

# cd ../SpoolEase-Debug/improve-mqtt
# git status
#
# echo git add .
# echo git commit -m "1"
# echo git push
#
# echo that is assuming you executed ". ./deploy.sh"

