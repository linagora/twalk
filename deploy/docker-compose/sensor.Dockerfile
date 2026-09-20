# Sensor image for the reference deployment (see compose.yaml). Multi-stage:
# the Rust build happens in the official toolchain image, the runtime is a
# slim Debian carrying the binary alone. BuildKit cache mounts keep the cargo
# registry and the target directory across builds: the first build compiles
# the whole dependency tree (several minutes), later builds only recompile
# the sensor crate itself.
#
# The build context is the repository root, not the sensor crate: cargo
# resolves a package's whole dependency graph, dev dependencies included, so
# the shared test harness (`tests/harness/`) has to be in the context even
# though a release build never compiles it. Without it cargo stops at
# "failed to load source for dependency twalk-test-harness" before compiling
# anything. The root `.dockerignore` keeps that wider context small.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY sensor sensor
COPY tests/harness tests/harness
# The shared consent cache (#273): a path dependency of the Sensor.
COPY consent-cache consent-cache
WORKDIR /src/sensor
RUN --mount=type=cache,id=twalk-sensor-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=twalk-sensor-target,target=/src/sensor/target \
    cargo build --release --locked \
    && cp target/release/twalk-sensor /usr/local/bin/twalk-sensor

FROM debian:bookworm-slim
# ca-certificates lets the same image talk to an https homeserver when the
# sensor is pointed outside this compose network; libsqlite3 is the dynamic
# dependency of the SDK's state and crypto stores.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system sensor \
    && mkdir -p /data \
    && chown sensor:sensor /data
COPY --from=build /usr/local/bin/twalk-sensor /usr/local/bin/twalk-sensor
# /data is the default SENSOR_STATE_DIR; a fresh named volume inherits this
# ownership.
USER sensor
ENTRYPOINT ["twalk-sensor"]
