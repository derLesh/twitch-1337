# Dockerfile for twitch-1337 bot
# Uses cargo-chef for efficient dependency caching
# Final image is FROM scratch with statically linked musl binary

FROM docker.io/lukemathwalker/cargo-chef:latest-rust-1@sha256:00c3c07c51d092325df88f0df2d626cd4302e12933f179ba154509cc314d6c2a AS base

WORKDIR /app
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=clang
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold"
# hadolint ignore=DL3008
RUN apt-get update \
  && apt-get install --no-install-recommends --assume-yes musl-tools mold clang ca-certificates \
  && rm -rf /var/lib/apt/lists/* \
  && rustup target add x86_64-unknown-linux-musl

# 1. Planner stage - generates dependency recipe
FROM base AS planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# 2. Cacher stage - builds dependencies only
FROM base AS cacher

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --target x86_64-unknown-linux-musl --recipe-path recipe.json

# 3. Builder stage - builds the application
FROM base AS builder

# Build-arg: short commit SHA of the source tree. Required because
# .dockerignore strips .git/, so the web crate's build.rs cannot derive
# it itself. Defaults to "unknown" if the caller does not pass one.
ARG GIT_SHA=unknown
ENV GIT_SHA=${GIT_SHA}

# Copy over cached dependencies
COPY --from=cacher /app/target target
COPY --from=cacher /usr/local/cargo /usr/local/cargo

# Copy source code and embedded data
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates

# Build the application with musl target (produces fully static binary)
RUN cargo build -p twitch-1337 --release --target x86_64-unknown-linux-musl

# 4. Runtime stage - minimal FROM scratch image
FROM scratch
LABEL org.opencontainers.image.description="Rust Twitch IRC bot: 1337 tracker, community pings, AI/flight commands, and scheduled messages — single persistent IRC connection with broadcast-based handler routing."
ENV DATA_DIR=/data

# Copy CA bundle so rustls-platform-verifier can load system roots
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

# Copy the static binary
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/twitch-1337 /twitch-1337

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD ["/twitch-1337", "--healthcheck"]

# Run the bot
ENTRYPOINT ["/twitch-1337"]
