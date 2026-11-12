# micro-dsql container image.
#
#   docker build -t micro-dsql .
#   docker run --rm -p 5432:5432 micro-dsql
#   psql "host=127.0.0.1 port=5432 user=admin dbname=postgres password=anything sslmode=disable"
#
# The binary is linked statically against musl, so the runtime image is `scratch`: no shell,
# no libc, no package manager. Builds for the builder's own architecture (amd64 or arm64).
#
# The build runs on a glibc (Debian) host and cross-compiles to the musl target: build
# scripts stay dynamic, which bindgen needs to load libclang for pg_query, while the
# final binary is fully static.

ARG RUST_VERSION=1
ARG DEBIAN_VERSION=bookworm

# ---------------------------------------------------------------------------
# Build stage
# ---------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-slim-${DEBIAN_VERSION} AS build

# clang/libclang for pg_query's bindgen, musl-tools for the musl C compiler,
# cmake for aws-lc-sys, git for the Turso git dependency.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      clang libclang-dev musl-tools musl-dev cmake make perl git ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add "$(uname -m)-unknown-linux-musl"

WORKDIR /src

# 1. Dependency layer: build the (heavy) dependency tree against stub sources, so that
#    source-only changes reuse it.
COPY Cargo.toml Cargo.lock ./
COPY crates/policy/Cargo.toml crates/policy/
COPY crates/server/Cargo.toml crates/server/
COPY crates/micro-dsql/Cargo.toml crates/micro-dsql/
COPY docker/build.sh /usr/local/bin/build-micro-dsql
RUN mkdir -p crates/policy/src crates/server/src crates/micro-dsql/src \
 && touch crates/policy/src/lib.rs crates/server/src/lib.rs \
 && echo 'fn main() {}' > crates/micro-dsql/src/main.rs \
 && build-micro-dsql \
 && rm -rf crates

# 2. Real sources.
COPY crates crates
RUN touch crates/policy/src/lib.rs crates/server/src/lib.rs crates/micro-dsql/src/main.rs \
 && build-micro-dsql /micro-dsql \
 && mkdir -p /rootfs/data /rootfs/tmp

# ---------------------------------------------------------------------------
# Runtime stage
# ---------------------------------------------------------------------------
FROM scratch

COPY --from=build /micro-dsql /micro-dsql
COPY LICENSE /licenses/LICENSE
COPY crates/server/LICENSE-turso /licenses/LICENSE-turso
# Writable directories: /data for `--data /data/<file>` (mount a volume to keep it).
COPY --from=build --chown=65532:65532 /rootfs/data /data
COPY --from=build --chown=65532:65532 /rootfs/tmp /tmp

USER 65532:65532
EXPOSE 5432
ENTRYPOINT ["/micro-dsql"]
# Listen on all interfaces inside the container; the in-memory database is the default.
CMD ["--listen", "0.0.0.0:5432"]
