#!/usr/bin/env bash
# Every DE2 example built by two chipsmith binaries, the .rbf compared byte
# for byte: a change that shouldn't touch the DE2 flow, checked.
#
#   bench/de2-identical.sh <old chipsmith> <new chipsmith> <out dir>
#
# Each example is built from a fresh copy (with its `../` siblings) by
# each binary, no cache. Prints `<example> identical|DIFFERS|FAILED` per
# example and a count; exits 1 on any difference or failure. Run it in the
# devshell (CHIPSMITH_PRJXRAY_DB and the Cyclone II database set).
set -uo pipefail
old=$(realpath "$1") new=$(realpath "$2") out=$3
root=$(dirname "$(dirname "$(realpath "$0")")")
mkdir -p "$out"
bad=0 n=0
for toml in "$root"/examples/*/chipsmith.toml; do
    grep -qi 'EP2C35' "$toml" || continue
    name=$(basename "$(dirname "$toml")")
    n=$((n + 1))
    for which in old new; do
        s=$out/$which/$name
        rm -rf "$s"
        mkdir -p "$s/examples"
        cp -r "$root/examples/$name" "$s/examples/"
        rm -rf "$s/examples/$name/build"
        for sib in $(grep -o '"\.\./[^/"]*' "$toml" | cut -c5- | sort -u); do
            cp -r "$root/examples/$sib" "$s/examples/"
        done
        bin=$old
        [[ $which == new ]] && bin=$new
        CHIPSMITH_CACHE=0 "$bin" -C "$s/examples/$name" build > "$s/build.log" 2>&1
    done
    a=$(ls "$out/old/$name/examples/$name"/build/*.rbf 2>/dev/null | head -1)
    b=$(ls "$out/new/$name/examples/$name"/build/*.rbf 2>/dev/null | head -1)
    if [[ -z $a || -z $b ]]; then
        echo "$name FAILED"
        bad=$((bad + 1))
    elif cmp -s "$a" "$b"; then
        echo "$name identical"
    else
        echo "$name DIFFERS"
        bad=$((bad + 1))
    fi
done
echo "$((n - bad)) of $n DE2 examples identical"
[[ $bad == 0 ]]
