# The collector's image for the reference deployment (see compose.yaml): the
# process that holds one OIDC grant for the owner's own accounts — a mailbox,
# a calendar — and publishes what involves other people on the bus (ADR
# 0033, #274).
#
# Multi-stage, like the Sensor's, the Gateway's and the clerk's. The build
# context is the repository root, because the crate's dependency graph names
# the shared consent cache (`consent-cache/`, #280: a participant in the
# owner's meeting is labelled by the decision about them) and the shared
# test harness (`tests/harness/`, a dev dependency cargo still needs
# present) as path dependencies.

FROM rust:1-bookworm AS build
# See clerk.Dockerfile for why the ceiling is four.
ARG CARGO_BUILD_JOBS=4
WORKDIR /src
COPY collector collector
COPY consent-cache consent-cache
COPY tests/harness tests/harness
# The contract fixture `status.rs` embeds with `include_str!` for its test,
# and the definitions the crate may read: the path the source names.
COPY contracts/cloudevents/v1 contracts/cloudevents/v1
WORKDIR /src/collector
# Stamped to "now" after the COPY, for the reason clerk.Dockerfile gives.
RUN find . ../consent-cache ../tests/harness ../contracts -type f \
        \( -name '*.rs' -o -name '*.toml' -o -name '*.lock' -o -name '*.json' \) \
        -exec touch {} +
RUN --mount=type=cache,id=twalk-collector-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=twalk-collector-target,target=/src/collector/target \
    cargo build --release --locked \
    && cp target/release/twalk-collector /usr/local/bin/twalk-collector

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system collector \
    && mkdir -m 0700 /run/collector /data \
    && chown collector:collector /run/collector /data
COPY --from=build /usr/local/bin/twalk-collector /usr/local/bin/twalk-collector
COPY deploy/docker-compose/collector-entrypoint.sh /usr/local/bin/collector-entrypoint
RUN chmod 0755 /usr/local/bin/collector-entrypoint
# The entrypoint drops to `collector` after copying the client secret — a
# host file the operator's own account owns — into /run/collector for that
# account, the way the clerk's does with its key.
ENTRYPOINT ["/usr/local/bin/collector-entrypoint"]
