#!/bin/bash
set -euo pipefail

# lld embeds rustc's randomly staged Natvis and synthesized object paths in
# the PDB. Make each path a function of its contents before /Brepro derives
# the PDB signature embedded in the PE.
stable_root=/tmp/spice-link-inputs

stable_input() { # env-policy: allow
    local source=$1
    local digest stable temporary
    digest=$(sha256sum -- "$source")
    digest=${digest%% *}
    stable="$stable_root/$digest-${source##*/}"
    mkdir -p -- "$stable_root"
    if [[ ! -e "$stable" ]]; then
        temporary="$stable.$$"
        cp -- "$source" "$temporary"
        chmod 0444 "$temporary"
        mv -f -- "$temporary" "$stable"
    fi
    printf '%s\n' "$stable"
}

arguments=()
for argument in "$@"; do
    # env-policy: allow
    case "$argument" in
        /NATVIS:* | /natvis:*)
            arguments+=("/NATVIS:$(stable_input "${argument#*:}")")
            ;;
        */rustc*/symbols.o)
            arguments+=("$(stable_input "$argument")") # env-policy: allow
            ;;
        *)
            arguments+=("$argument") # env-policy: allow
            ;;
    esac
done

exec /usr/lib/llvm-19/bin/lld-link \
    /Brepro \
    /winsysroot:/opt/spice/msvc/sysroot \
    /vfsoverlay:/opt/spice/msvc/overlay.json \
    "${arguments[@]}"
