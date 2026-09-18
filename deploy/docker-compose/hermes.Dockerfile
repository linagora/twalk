# Hermes image for the reference deployment (see compose.yaml): the consumer
# runtime, plus the two things it needs to start a persona inside a
# deployment — a Docker client, and the wrapper that carries the environment
# the runtime constructs across the container wall.
#
# Multi-stage, like the Sensor's and the Gateway's: the Rust build happens in
# the official toolchain image, the runtime is a slim Debian carrying the
# binary alone. BuildKit cache mounts keep the cargo registry and the target
# directory across builds.
#
# The build context is the repository root, not the hermes crate: cargo
# resolves a package's whole dependency graph, dev dependencies included, so
# the shared test harness (`tests/harness/`) has to be in the context even
# though a release build never compiles it — the same reason
# sensor.Dockerfile gives. The root `.dockerignore` keeps that wider context
# small.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY hermes hermes
COPY tests/harness tests/harness
WORKDIR /src/hermes
# Cargo decides what to rebuild by comparing mtimes against the artifacts in
# `target/`, and `target/` is a cache mount shared by every build of this
# image — across worktrees, and across concurrent runs. Stamping this tree's
# sources to "now" after the COPY keeps the cache's value and removes the
# trap an artefact from another tree would otherwise set
# (companion-gateway.Dockerfile tells the story of the image that shipped).
RUN find . -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.lock' \) -exec touch {} +
RUN --mount=type=cache,id=twalk-hermes-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=twalk-hermes-target,target=/src/hermes/target \
    cargo build --release --locked \
    && cp target/release/twalk-hermes /usr/local/bin/twalk-hermes

FROM debian:bookworm-slim
# The Docker CLI, and nothing else of Docker: the daemon this talks to is the
# host's, through the socket compose mounts (ADR 0023). Taken from the
# official CLI image — a statically linked binary, so it runs on Debian as
# happily as on the Alpine it was built for — rather than from `docker.io`,
# which would install a second daemon this image must never run. Pinned, as
# every other image in this deployment is.
COPY --from=docker:28-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=build /usr/local/bin/twalk-hermes /usr/local/bin/twalk-hermes
# The argv every persona in HERMES_PERSONAS names, and the entrypoint that
# decides whether this deployment has been given a model to reason with.
COPY deploy/docker-compose/run-persona-image.sh /usr/local/bin/run-persona
COPY deploy/docker-compose/hermes-entrypoint.sh /usr/local/bin/hermes-entrypoint
RUN chmod 0755 /usr/local/bin/run-persona /usr/local/bin/hermes-entrypoint

# This image runs as root, and unusually for this repository that is not an
# oversight: it holds the host's Docker socket, which is root on the host
# whatever uid asks for it (ADR 0023, docs/architecture/security-model.md).
# Dropping to an unprivileged user that must then be handed the host's
# `docker` group back — a gid that differs on every machine — would be
# ceremony around a door that is already open, and it would fail in a
# confusing way on the first host whose gid did not match. An operator who
# wants that door narrower points HERMES_DOCKER_SOCKET at a rootless daemon's
# socket, which is the mitigation the ADR names.
ENTRYPOINT ["/usr/local/bin/hermes-entrypoint"]
