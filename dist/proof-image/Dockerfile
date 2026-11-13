# syntax=docker/dockerfile:1.7

FROM --platform=linux/amd64 docker.io/library/rust@sha256:858f8882676b21c915fb5fab0261fc80d47c21f00faf9f610db89f966978eca6
ARG spice_vendor_set
LABEL org.spice.vendor-set="${spice_vendor_set}"

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && \
    apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        docker.io \
        git \
        musl-tools \
        pkg-config

RUN rustup component add clippy rustfmt && \
    rustup target add wasm32-unknown-unknown x86_64-unknown-linux-musl

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo install --locked --root /opt/spice/tools --version 0.22.1 trunk && \
    cargo install --locked --root /opt/spice/tools --version 0.2.126 wasm-bindgen-cli

# Trunk downloads `wasm-opt` the first time it optimizes a release module, so the
# appliance is offline only once that binary is already here. Offline Trunk asks
# for `wasm-opt` at any version and resolves it from `PATH`, never from the
# versioned tool cache it populates when it downloads one itself, so this
# installs alongside the other pinned tools rather than shadowing a private
# cache layout. One file is installed because the release links `libbinaryen`
# statically and ships the rest of `bin` for tools nothing here runs.
ADD --checksum=sha256:b5bf1f0eaf17c63ee588ff7a5954dc8f6ce2c26989051c66f24dfe9ece3e46db \
    https://github.com/WebAssembly/binaryen/releases/download/version_131/binaryen-version_131-x86_64-linux.tar.gz \
    /tmp/binaryen.tar.gz

RUN tar -xzf /tmp/binaryen.tar.gz --strip-components=2 \
        -C /opt/spice/tools/bin binaryen-version_131/bin/wasm-opt && \
    rm /tmp/binaryen.tar.gz && \
    /opt/spice/tools/bin/wasm-opt --version

COPY vendor /opt/spice/vendor
# Cargo reads a config from the working directory and every parent up to the
# filesystem root, so the root is the one location every Cargo in this image
# reads. The proof boundary keeps `CARGO_HOME` as an empty tmpfs; this config is
# therefore the immutable route to the packaged source replacement. It follows
# the installs above, which are the only steps allowed to reach the network.
COPY cargo-config.toml /.cargo/config.toml
COPY cargo-wrapper.sh /opt/spice/bin/cargo

RUN chmod 0755 /opt/spice/bin/cargo && \
    ln -s /usr/local/cargo/bin/rustfmt /opt/spice/bin/rustfmt && \
    git config --system safe.directory '*' && \
    /opt/spice/bin/cargo --version && \
    /opt/spice/bin/cargo clippy --version && \
    /opt/spice/bin/rustfmt --version && \
    /opt/spice/tools/bin/trunk --version && \
    /opt/spice/tools/bin/wasm-bindgen --version && \
    rustup target list --installed

ENV CARGO_NET_OFFLINE=true \
    PATH=/opt/spice/bin:/opt/spice/tools/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    RUSTUP_TOOLCHAIN=1.97.1 \
    TMPDIR=/cargo-target/tmp \
    TRUNK_OFFLINE=true

WORKDIR /workspace
