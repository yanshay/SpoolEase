base_target="spoolease-bin"
path_in_base_target="/bins/0.8"
page="debug.html"
rel_train="debug"

product="console"

source ./deploy-vars.sh || { return 1 2>/dev/null || exit 1; }

#----

replace_dir=""
case "$product" in
  "console") replace_dir="./deploy-fix-html.sh";;
  "scale") replace_dir="../console/core/deploy-fix-html.sh";;
  *) echo "Not a valid product"; exit 1;;
esac

pushd "$xtask_dir" || { return 1 2>/dev/null || exit 1; }
cargo xtask ota build --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/${rel_train}" || { popd; return 1 2>/dev/null || exit 1; }
cargo xtask web-install build --layout updater-ota --input "$proj_dir" --output "$base_target_dir${path_in_base_target}/${product}/${rel_train}" --support-dir "$base_target_dir/bins/0.8/console/web-install" --updater-web-install "$base_target_dir/bins/0.8/updater/web-install" || { popd; return 1 2>/dev/null || exit 1; }
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
