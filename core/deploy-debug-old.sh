proj_dir=$(pwd)
source ../../deploy-config.sh || { return 1 2>/dev/null || exit 1; }

path=$(pwd)
debug_dir=""
while [ "$path" != "/" ]; do
    if [ -d "$path/SpoolEase-Debug" ]; then
        debug_dir="$path/SpoolEase-Debug"
        break
    fi
    path=$(dirname "$path")
done

if [ -z "$debug_dir" ]; then
    echo "SpoolEase-Debug not found" >&2
    exit 1
fi

pushd "$xtask_dir" || { return 1 2>/dev/null || exit 1; }
cargo xtask ota build --input "$proj_dir" --output "$debug_dir/0.8/console" || { popd; return 1 2>/dev/null || exit 1; }
cargo xtask web-install build --layout dual-ota --input "$proj_dir" --output "$debug_dir/0.8/console" || { popd; return 1 2>/dev/null || exit 1; }
popd

replace=$(grep '^version' Cargo.toml | sed -E 's/version *= *"[^"]*-([^"]+)".*/\1/')

./deploy-fix-html.sh "$debug_dir/0.6/alpha.html" console "$replace"

# cd ../SpoolEase-Debug/improve-mqtt
# git status
#
# echo git add .
# echo git commit -m "1"
# echo git push
#
# echo that is assuming you executed ". ./deploy.sh"
