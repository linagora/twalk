# The clerk's image for the reference deployment (see compose.yaml): the
# binary that writes onto the owner's Buzz relay what the bus says, under a
# Nostr key of its own, holding no state (ticket #265, ADR 0035).
#
# Multi-stage, like the Sensor's, the Gateway's and Hermes's: the Rust build
# happens in the official toolchain image, the runtime is a slim Debian
# carrying the binary and its entrypoint. BuildKit cache mounts keep the
# cargo registry and the target directory across builds.
#
# The build context is the repository root, not the clerk crate: cargo
# resolves a package's whole dependency graph, dev dependencies included, so
# the shared test harness (`tests/harness/`) has to be in the context even
# though a release build never compiles it — the same reason
# sensor.Dockerfile gives. The root `.dockerignore` keeps that wider context
# small.

FROM rust:1-bookworm AS build
# Cargo's default is one job per core, and a build inside the daemon sees
# none of the caps the host's own shell sets. On a host that also runs its
# owner's services, a release build of the whole dependency tree at that
# width is what pushes it into swap, so the ceiling is four — overridable
# with `--build-arg CARGO_BUILD_JOBS=<n>` (or `build.args` in a compose
# override) on a machine with room. It costs a cold build minutes and a warm
# one nothing.
ARG CARGO_BUILD_JOBS=4
WORKDIR /src
COPY clerk clerk
COPY tests/harness tests/harness
WORKDIR /src/clerk
# Cargo decides what to rebuild by comparing mtimes against the artifacts in
# `target/`, and `target/` is a cache mount shared by every build of this
# image — across worktrees, and across concurrent runs. Stamping this tree's
# sources to "now" after the COPY keeps the cache's value and removes the
# trap an artefact from another tree would otherwise set
# (companion-gateway.Dockerfile tells the story of the image that shipped).
RUN find . -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.lock' \) -exec touch {} +
RUN --mount=type=cache,id=twalk-clerk-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=twalk-clerk-target,target=/src/clerk/target \
    cargo build --release --locked \
    && cp target/release/twalk-clerk /usr/local/bin/twalk-clerk

FROM debian:bookworm-slim
# ca-certificates, as every runtime image here carries; curl is what the
# compose healthcheck probes the clerk's own health endpoint with. The
# unprivileged account the binary runs as, and the one directory it may
# write: where the entrypoint puts the copy of the key it hands the binary
# (see clerk-entrypoint.sh).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system clerk \
    && mkdir -m 0700 /run/clerk \
    && chown clerk:clerk /run/clerk
COPY --from=build /usr/local/bin/twalk-clerk /usr/local/bin/twalk-clerk
# The entrypoint that decides whether this deployment has been given a relay
# to write to, and which hands the binary its key.
COPY deploy/docker-compose/clerk-entrypoint.sh /usr/local/bin/clerk-entrypoint
RUN chmod 0755 /usr/local/bin/clerk-entrypoint

# No `USER clerk` here, and the binary still runs as `clerk`: the entrypoint
# drops to that account (setpriv) before it starts the binary. The line is
# absent because of the key. The key file is a bind mount of a host file the
# operator's own account owns, mode 0600 — `provision-nostr-key.sh` writes
# it that way and the binary refuses anything looser, since the key signs
# everything the clerk says — and no fixed uid this image could pick is the
# owner of a file on a host it has never seen. So the entrypoint, as root,
# copies the key into /run/clerk with `clerk` as its owner, and the binary
# reads that copy as an unprivileged process. Root is held for that one
# `install` and nothing else; a `USER` line would trade it for a key the
# process cannot open, reported as "Permission denied" on the first real run.
ENTRYPOINT ["/usr/local/bin/clerk-entrypoint"]
