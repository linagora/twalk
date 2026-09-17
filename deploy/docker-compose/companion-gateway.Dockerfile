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
# exists, this stage emits the holding page, so the image's shape is already
# the final one and only the build command changes when the app lands.
#
# When it lands, its adapter-static must be configured with
# `fallback: '200.html'` — the name the Gateway serves any client-side route
# with (GATEWAY_FALLBACK_FILE), and the name the adapter's own documentation
# recommends over index.html, which would collide with the prerendered
# homepage.
FROM node:22-bookworm-slim AS companion
WORKDIR /src
COPY companion companion
COPY companion-gateway/holding-page holding-page
RUN set -eu; \
    if [ -f companion/package.json ]; then \
      cd companion && npm ci && npm run build && cp -r build /companion-dist; \
    else \
      echo "no Companion build in companion/: shipping the holding page"; \
      cp -r holding-page /companion-dist; \
      cp holding-page/index.html /companion-dist/200.html; \
    fi

FROM rust:1-bookworm AS build
WORKDIR /src
COPY companion-gateway companion-gateway
COPY tests/harness tests/harness
# The revision the health endpoint reports. The build context carries no
# .git (see .dockerignore), so pass it in — e.g.
# `docker compose build --build-arg TWALK_BUILD_REVISION=$(git describe --always --dirty)`
# — or accept `unknown`: the version handshake compares the package version,
# which is always exact.
ARG TWALK_BUILD_REVISION=unknown
ENV TWALK_BUILD_REVISION=${TWALK_BUILD_REVISION}
WORKDIR /src/companion-gateway
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
