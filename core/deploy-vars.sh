proj_dir=$(pwd)

source "$(dirname "${BASH_SOURCE[0]}")/../../deploy-config.sh" || return 1

path=$(pwd)
base_target_dir=""
while [ "$path" != "/" ]; do
    if [ -d "$path/hamster/${base_target}" ]; then
        base_target_dir="$path/hamster/${base_target}"
        break
    fi
    path=$(dirname "$path")
done
if [ -z "$base_target_dir" ]; then
    echo "${base_target} not found" >&2
    exit 1
fi
