#!/bin/sh
set -eu

# The image fixes TMPDIR under the target directory and the sealed container
# contract refuses to start unless that directory is exactly /cargo-target, so
# nothing here is variable. Only the directory itself has to be made, because
# the proof boundary mounts an empty tmpfs over it. Making it is now this
# script's whole job: the vendored sources are declared at /.cargo/config.toml,
# which Cargo reads on its own, so nothing here supplies them.
mkdir -p /cargo-target/tmp

exec /usr/local/cargo/bin/cargo "$@"
