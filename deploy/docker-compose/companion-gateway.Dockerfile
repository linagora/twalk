# Companion Gateway image for the reference deployment (see compose.yaml).
# Three stages: a Node stage that produces the Companion's static files, the
# Rust toolchain image that builds the Gateway, and a slim Debian runtime
# carrying the binary and those files alone. Nothing built is committed.
# BuildKit cache mounts keep the cargo registry and the target directory
# across builds: the first build compiles the whole dependency tree, later
# builds only recompile the Gateway crate itself.
#
# The build context is the repository root, not the Gateway crate: cargo
# resolves a package's whole dependency graph, dev dependencies included, so
# the shared test harness (`tests/harness/`) has to be in the context even
# though a release build never compiles it — the same reason
# sensor.Dockerfile gives. The root `.dockerignore` keeps that wider context
# small.

# The Companion is a SvelteKit static export (its own lot). Until that build
# exists, this stage emitted a holding page; since ticket #66 it builds the
# app, and `companion-gateway/holding-page/` is no longer referenced from
# anywhere.
#
# The build is `companion/`'s own and configured there: `adapter-static` with
# `fallback: '200.html'` — the name the Gateway serves any client-side route
# with (GATEWAY_FALLBACK_FILE), and the name the adapter's documentation
# recommends over index.html, which would collide with the prerendered
# homepage — `precompress: true` for the `.br` and `.gz` siblings the runtime's
# `ServeFile` looks for, and `trailingSlash: 'never'`, which writes a
# prerendered page as `<path>.html`: step 2 of the resolution order in
# `companion-gateway/src/static_files.rs`.
#
# `npm ci`, not `npm install`: the lockfile is committed, so the image builds
# the dependency tree the tests ran against. The generated Gateway client
# (`companion/src/lib/api/`) is committed too, which is what lets this stage
# copy `companion/` alone — it has no sibling directory to generate from, and
# `npm run api:check` outside the image is what keeps it honest.
FROM node:22-bookworm-slim AS companion
WORKDIR /src/companion
COPY companion .
RUN npm ci --no-audit --no-fund \
    && npm run build \
    && cp -r build /companion-dist \
    && test -f /companion-dist/200.html \
    && test -f /companion-dist/index.html

FROM rust:1-bookworm AS build
WORKDIR /src
COPY companion-gateway companion-gateway
COPY tests/harness tests/harness
# The contract's shared definitions are compiled into the binary
# (`include_str!`): the kinds of connection have one authority (#268), and
# the Gateway carries it rather than a copy.
COPY contracts/cloudevents/v1/definitions contracts/cloudevents/v1/definitions
# The revision the health endpoint reports. The build context carries no
# .git (see .dockerignore), so pass it in — e.g.
# `docker compose build --build-arg TWALK_BUILD_REVISION=$(git describe --always --dirty)`
# — or accept `unknown`: the version handshake compares the package version,
# which is always exact.
ARG TWALK_BUILD_REVISION=unknown
ENV TWALK_BUILD_REVISION=${TWALK_BUILD_REVISION}
WORKDIR /src/companion-gateway
# Cargo decides what to rebuild by comparing mtimes against the artifacts in
# `target/`, and `target/` is a cache mount shared by every build of this image
# — across worktrees, and across concurrent runs. An artifact written by
# *another* tree can therefore be newer than this tree's sources, and cargo
# then reuses a binary compiled from source that is not in this build context.
#
# That is not hypothetical: it shipped an image whose embedded `openapi.yaml`
# (`include_str!`, so an ordinary mtime-tracked dependency) was a previous
# branch's, and the deployment test compared the served description against the
# repository's and failed — correctly, on a binary nobody could locate the
# source of. Stamping every source to "now" after the COPY keeps the cache's
# value and removes the trap: this tree's sources are always newer than
# anything already in it.
RUN find . -type f \
        \( -name '*.rs' -o -name '*.toml' -o -name '*.yaml' -o -name '*.lock' \) \
        -exec touch {} +
RUN --mount=type=cache,id=twalk-gateway-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=twalk-gateway-target,target=/src/companion-gateway/target \
    cargo build --release --locked \
    && cp target/release/twalk-companion-gateway /usr/local/bin/twalk-companion-gateway

FROM debian:bookworm-slim
# ca-certificates lets the same image talk to an https homeserver when the
# Gateway is pointed outside this compose network; curl is what the compose
# healthcheck probes the Gateway's own health endpoint with.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system gateway \
    && mkdir -p /data \
    && chown gateway:gateway /data
COPY --from=build /usr/local/bin/twalk-companion-gateway /usr/local/bin/twalk-companion-gateway
# The Companion's origin content. An operator serving their own build mounts
# it over this path (or points GATEWAY_STATIC_DIR elsewhere); an empty or
# absent directory is not fatal — the origin answers a clear 404 while health
# and metrics stay up.
COPY --from=companion /companion-dist /srv/companion
# /data is the default GATEWAY_STATE_DIR — the session store's home. A fresh
# named volume mounted there inherits the ownership set above, so the
# non-root user can write it (the Sensor's image does the same).
# Defaults for a container: the origin listens on all interfaces inside the
# container's own network namespace, and serves the files above.
ENV GATEWAY_LISTEN=0.0.0.0:8080 \
    GATEWAY_STATIC_DIR=/srv/companion \
    GATEWAY_STATE_DIR=/data
EXPOSE 8080
USER gateway
ENTRYPOINT ["twalk-companion-gateway"]
