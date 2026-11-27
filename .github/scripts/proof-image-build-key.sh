#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" != 1 ]]; then
  echo 'usage: proof-image-build-key.sh OUTPUT.json' >&2
  exit 2
fi

output="$1"

# Each digest is a plain assignment so a missing input stops the script.
tree_digest() { # env-policy: allow
  [[ -n "$(git ls-tree -r --name-only HEAD -- "$@")" ]] || return 1
  git ls-tree -r -z HEAD -- "$@" | sha256sum | cut -d' ' -f1
}

# env-policy: allow
cargo_lock="$(sha256sum Cargo.lock | cut -d' ' -f1)"
# env-policy: allow
cargo_config="$(sha256sum .cargo/config.toml | cut -d' ' -f1)"
# env-policy: allow
rust_toolchain="$(sha256sum rust-toolchain.toml | cut -d' ' -f1)"
# env-policy: allow
proof_image="$(tree_digest dist/proof-image proof vendor-set xtask/src/proof_image.rs)"
# env-policy: allow
contract="$(tree_digest .github/scripts/proof-image-build-key.sh .github/workflows/proof-image.yml prepared wire/src/manifest.rs xtask/Cargo.toml xtask/src/lib.rs xtask/src/main.rs xtask/src/prepared_artifact.rs)"

# env-policy: allow
jq -n \
  --arg cargo_lock "sha256:$cargo_lock" \
  --arg cargo_config "sha256:$cargo_config" \
  --arg rust_toolchain "sha256:$rust_toolchain" \
  --arg proof_image "sha256:$proof_image" \
  --arg contract "sha256:$contract" \
  '{cargo_lock:$cargo_lock,cargo_config:$cargo_config,rust_toolchain:$rust_toolchain,proof_image:$proof_image,contract:$contract}' \
  > "$output"
