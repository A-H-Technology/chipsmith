#!/usr/bin/env bash
# Have Quartus assemble a built DE2 project's placement and routing, and
# compare its RBF with chipsmith's, byte for byte.
#
#   bench/quartus-validate.sh <project dir>...
#
# After `chipsmith -C <project dir> build`: cyclone2-re's `cram
# quartus-export` turns build/routed.json (pin settings, I/O registers and
# the unused-pin mode included) into Quartus input, its oracle assembles it
# (Quartus II 13.0sp1), and build/<name>.rbf must come out the same. Prints
# one line per project; exits 1 if any differ. CYCLONE2_RE points at a
# cyclone2-re checkout whose tools/cram is built.
set -euo pipefail
re=${CYCLONE2_RE:-$(dirname "$(realpath "$0")")/../../cyclone2-re}
status=0
for project in "$@"; do
    project=$(realpath "$project")
    name=$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$project/chipsmith.toml" | head -1)
    work="$project/build/quartus"
    rm -rf "$work"
    mkdir -p "$work/build"
    cp "$project/build/routed.json" "$work/build/routed.json"
    cp "$project/build/$name.rbf" "$work/build/top.rbf"
    if "$re/tools/validate.sh" "$work" > "$work/validate.log" 2>&1 \
        && grep -q "BYTE-IDENTICAL" "$work/validate.log"; then
        echo "$(basename "$project"): byte-identical"
    else
        echo "$(basename "$project"): DIFFERS ($(grep -h '^validate:' "$work/validate.log" | head -1); see $work/validate.log)"
        status=1
    fi
done
exit $status
