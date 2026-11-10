#!/usr/bin/env bash
# Every spec bench/ring/specs.py writes, built, loaded and read on the DE2
# (bench/ring/run.sh), REPEATS times each with a fresh load in between.
#
#   bench/ring/measure.sh <out dir> [spec name ...]
set -euo pipefail
root=$(dirname "$(dirname "$(dirname "$(realpath "$0")")")")
if [[ -z "${CHIPSMITH_CYCLONE2_DB:-}" ]]; then
    cd "$root"
    exec nix develop "$root" --command "$0" "$@"
fi
out=$(realpath -m "$1")
shift
python3 "$root/bench/ring/specs.py" "$out/specs" > /dev/null
names=("$@")
[[ ${#names[@]} -gt 0 ]] || names=($(cd "$out/specs" && ls *.json | sed 's/\.json$//'))
for name in "${names[@]}"; do
    for rep in $(seq 1 "${REPEATS:-2}"); do
        echo "== $name $rep"
        "$root/bench/ring/run.sh" "$out/specs/$name.json" "$out/$name/$rep" > /dev/null
    done
done
