#!/bin/sh
set -eu

root=$(git rev-parse --show-toplevel) # env-policy: allow
cd "$root" # env-policy: allow

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
    printf 'the native proof runner is sealed on native arm64 macOS only\n' >&2
    exit 65
fi
if ! git diff-index --quiet HEAD --; then
    printf 'the native proof runner is sealed from tracked source at HEAD\n' >&2
    exit 65
fi
untracked=$(git ls-files --others --exclude-standard | sed \
    -e '/^dist\/stage\/objects\//d' \
    -e '/^dist\/stage\/targets\//d')
if [ -n "$untracked" ]; then # env-policy: allow
    printf 'untracked source is outside release output:\n%s\n' "$untracked" >&2 # env-policy: allow
    exit 65
fi


# A clean hosted runner has no installed Spice appliance yet. Its bootstrap
# step may name the exact selected toolchain's Cargo executable directly; its
# sibling Rustc is then the only compiler this script admits. Every ordinary
# repository build continues through the admitted `spice dev cargo` surface.
# Refuse a relative or unusable override before it can affect vendor identity
# or the sealed runner.
dev_cargo() {
    if [ -n "${SPICE_PROOF_CARGO:-}" ]; then # env-policy: allow
        case "$SPICE_PROOF_CARGO" in # env-policy: allow
            /*) ;;
            *)
                printf 'SPICE_PROOF_CARGO must name an absolute executable\n' >&2
                exit 65
                ;;
        esac
        if [ ! -f "$SPICE_PROOF_CARGO" ] || [ ! -x "$SPICE_PROOF_CARGO" ]; then # env-policy: allow
            printf 'SPICE_PROOF_CARGO is not a regular executable: %s\n' "$SPICE_PROOF_CARGO" >&2 # env-policy: allow
            exit 65
        fi
        "$SPICE_PROOF_CARGO" "$@" # env-policy: allow
    else
        spice dev cargo -- "$@" # env-policy: allow
    fi
}
bundle="$root/target/proof-native-darwin" # env-policy: allow
# The unsealed and sealed runner builds share one release target directory, the
# optional first argument or this worktree's `target/`, so the sealed build
# recompiles only the runner after `SPICE_NATIVE_PINS` changes rather than its
# whole dependency graph a second time. A caller that already built release
# tools passes its own target directory to reuse that graph too.
target=${1:-$root/target} # env-policy: allow
bind=true
case "${2:-}" in # env-policy: allow
    '') ;;
    --seal-only) bind=false ;;
    *)
        printf 'usage: %s [TARGET] [--seal-only]\n' "$0" >&2
        exit 65
        ;;
esac
if [ "$#" -gt 2 ]; then
    printf 'usage: %s [TARGET] [--seal-only]\n' "$0" >&2
    exit 65
fi
vendor="$bundle/vendor" # env-policy: allow
rm -rf "$bundle" # env-policy: allow
mkdir -p "$bundle/bin" # env-policy: allow
dev_cargo vendor --locked --respect-source-config "$vendor" # env-policy: allow

# The unsealed build of the runner is the one tool that computes the identities
# the sealed build is pinned with, so the pins never come from a second
# implementation that could disagree with the one enforcing them.
unsealed() {
    dev_cargo run --locked -q --release --target-dir "$target" \
        -p proof --bin spice-proof-darwin-runner -- "$@" # env-policy: allow
}

if [ -n "${SPICE_PROOF_CARGO:-}" ]; then # env-policy: allow
    cargo_path=$SPICE_PROOF_CARGO # env-policy: allow
    rustc_path=$(dirname "$cargo_path")/rustc # env-policy: allow
    if [ ! -f "$rustc_path" ] || [ ! -x "$rustc_path" ]; then # env-policy: allow
        printf 'SPICE_PROOF_CARGO has no executable sibling rustc: %s\n' "$rustc_path" >&2 # env-policy: allow
        exit 65
    fi
else
    cargo_path=$(rustup which cargo) # env-policy: allow
    rustc_path=$(rustup which rustc) # env-policy: allow
fi
git_path=$(command -v git) # env-policy: allow
sandbox_path=/usr/bin/sandbox-exec
pins=$(printf '%s\n' \
    "source_commit = \"$(git rev-parse --verify 'HEAD^{commit}')\"" \
    "vendor_digest = \"$(unsealed closure-digest "$vendor")\"" \
    "sdk_path = \"$(xcrun --show-sdk-path)\"" \
    "developer_dir = \"$(xcode-select -p)\"" \
    "cargo_home = \"$(unsealed cargo-home)\"" \
    '' \
    '[cargo]' \
    "path = \"$cargo_path\"" \
    "digest = \"$(unsealed tool-digest "$cargo_path")\"" \
    '' \
    '[rustc]' \
    "path = \"$rustc_path\"" \
    "digest = \"$(unsealed tool-digest "$rustc_path")\"" \
    '' \
    '[git]' \
    "path = \"$git_path\"" \
    "digest = \"$(unsealed tool-digest "$git_path")\"" \
    '' \
    '[sandbox_exec]' \
    "path = \"$sandbox_path\"" \
    "digest = \"$(unsealed tool-digest "$sandbox_path")\"") # env-policy: allow

SPICE_NATIVE_PINS="$pins" \
    dev_cargo build --locked --release --target-dir "$target" \
    -p proof --bin spice-proof-darwin-runner # env-policy: allow

runner="$bundle/bin/spice-proof-darwin-runner" # env-policy: allow
install -m 555 "$target/release/spice-proof-darwin-runner" "$runner" # env-policy: allow
identity=$("$runner" runner-identity "$runner") # env-policy: allow
# The runner names its scratch root after the challenge, so this exercise mints
# a fresh one the way the host does. A fixed spelling collides with any seal
# running beside it, and one interrupted seal would leave a root behind that
# refuses every later seal on this machine for a reason that is not about it.
challenge=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n') # env-policy: allow
"$runner" spice-proof-native-runner-v1 "$identity" "$challenge" \
    cargo --version # env-policy: allow

printf '%s\n' "$pins" > "$bundle/pins.toml" # env-policy: allow

if [ "$bind" = false ]; then # env-policy: allow
    printf 'runner %s\nidentity %s\nsealed without proof.runner binding\n' "$runner" "$identity" # env-policy: allow
    exit 0
fi

# The runner is sealed for this worktree's HEAD and vendor tree, so it is bound
# here and nowhere else: `proof.runner` admits only the worktree layer, and the
# binding goes through the ordinary previewed, digest-applied config mutation.
plan=$(spice config proof.runner "$runner" | sed -n 's/^digest = "\([0-9a-f]*\)"$/\1/p') # env-policy: allow
if [ -z "$plan" ]; then # env-policy: allow
    printf 'the sealed runner could not be previewed into proof.runner\n' >&2
    exit 65
fi
spice config proof.runner "$runner" --apply="$plan" > /dev/null # env-policy: allow
printf 'runner %s\nidentity %s\nbound proof.runner (worktree)\n' "$runner" "$identity" # env-policy: allow
