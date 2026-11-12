#!/bin/sh
# Build a fully static micro-dsql for the native architecture's musl target.
# Usage: build.sh [OUTPUT]   (without OUTPUT only the build runs, e.g. to warm the cache)
set -eu

arch="$(uname -m)"
target="$arch-unknown-linux-musl"
triple_env="$(echo "$target" | tr 'a-z-' 'A-Z_')"
cc_env="$(echo "$target" | tr '-' '_')"

# C code (libpg_query, aws-lc) is compiled with the musl wrapper and linked statically.
export "CC_$cc_env=musl-gcc"
export "CARGO_TARGET_${triple_env}_RUSTFLAGS=-C target-feature=+crt-static"
# bindgen parses pg_query's headers for the musl target.
export "BINDGEN_EXTRA_CLANG_ARGS_$cc_env=-I/usr/include/$arch-linux-musl"

cargo build --release --locked -p micro-dsql --target "$target"

if [ $# -gt 0 ]; then
    cp "target/$target/release/micro-dsql" "$1"
    strip "$1"
    if ldd "$1" >/dev/null 2>&1; then
        echo "error: $1 is dynamically linked" >&2
        ldd "$1" >&2
        exit 1
    fi
fi
