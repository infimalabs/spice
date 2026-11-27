# syntax=docker/dockerfile:1.7

# The sealed proof image for the current vendor set, named by its deterministic
# local tag; the builder admits the tag's exact ID first and records it here.
ARG proof_image_tag
FROM ${proof_image_tag}
ARG proof_image
ARG msvc_generation
LABEL org.spice.proof-image="${proof_image}" \
      org.spice.windows-msvc-generation="${msvc_generation}"

# Debian's LLVM 19 drives the MSVC target: clang in cl mode, lld-link, the
# librarian, and the assembler. Exact package versions keep the link toolchain a
# property of this file rather than of the mirror's current answer.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && \
    apt-get install -y --no-install-recommends \
        clang-19=1:19.1.7-3+b1 \
        lld-19=1:19.1.7-3+b1 \
        llvm-19=1:19.1.7-3+b1

RUN rustup target add x86_64-pc-windows-msvc

# The CRT and Windows SDK closure admitted on the building host from the
# operator's recorded licence acceptance and the manifest digests in
# dist/windows/msvc.toml. It is copied, never downloaded here, so the image
# carries exactly the admitted generation and is never published.
COPY sysroot /opt/spice/msvc/sysroot
COPY cargo-config.toml /tmp/windows-cross-cargo-config.toml
COPY case-overlay.sh /tmp/windows-cross-case-overlay.sh

# lld-link runs with /Brepro: the PE timestamp and the PDB signature become a
# hash of the linked contents rather than the link time, so the same sources
# link to the same bytes whether the Cargo target is fresh or reused.
RUN mkdir /opt/spice/msvc/bin && \
    sh /tmp/windows-cross-case-overlay.sh /opt/spice/msvc/sysroot > /opt/spice/msvc/overlay.json && \
    rm /tmp/windows-cross-case-overlay.sh && \
    printf '#!/bin/sh\nexec /usr/lib/llvm-19/bin/clang --driver-mode=cl --target=x86_64-pc-windows-msvc /winsysroot /opt/spice/msvc/sysroot -vfsoverlay /opt/spice/msvc/overlay.json "$@"\n' \
        > /opt/spice/msvc/bin/cl.exe && \
    printf '#!/bin/sh\nexec /usr/lib/llvm-19/bin/lld-link /Brepro /winsysroot:/opt/spice/msvc/sysroot /vfsoverlay:/opt/spice/msvc/overlay.json "$@"\n' \
        > /opt/spice/msvc/bin/link.exe && \
    printf '#!/bin/sh\nexec /usr/lib/llvm-19/bin/llvm-lib "$@"\n' \
        > /opt/spice/msvc/bin/lib.exe && \
    printf '#!/bin/sh\nexec /usr/lib/llvm-19/bin/llvm-ml --m64 "$@"\n' \
        > /opt/spice/msvc/bin/ml64.exe && \
    chmod 0755 /opt/spice/msvc/bin/* && \
    cat /tmp/windows-cross-cargo-config.toml >> /.cargo/config.toml && \
    rm /tmp/windows-cross-cargo-config.toml && \
    printf '%s\n' "${msvc_generation}" > /opt/spice/msvc/generation && \
    rustup target list --installed | grep -qx x86_64-pc-windows-msvc && \
    /usr/lib/llvm-19/bin/lld-link --version

ENV PATH=/opt/spice/msvc/bin:/opt/spice/bin:/opt/spice/tools/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
