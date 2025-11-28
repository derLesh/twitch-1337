# Dockerfile for twitch-1337 bot
# Uses cargo-chef for efficient dependency caching
# Final image is FROM scratch with statically linked musl binary

FROM docker.io/lukemathwalker/cargo-chef:latest-rust-1 as base

WORKDIR /app
# Install musl tools for static linking
RUN apt-get update \
  && apt-get install --no-install-recommends --assume-yes musl=1.2.5-3 musl-dev=1.2.5-3 musl-tools=1.2.5-3 \
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

# Copy over cached dependencies
COPY --from=cacher /app/target target
COPY --from=cacher /usr/local/cargo /usr/local/cargo

# Copy source code
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY src src

# Build the application with musl target (produces fully static binary)
RUN cargo build --release --target x86_64-unknown-linux-musl

# 4. Runtime stage - minimal FROM scratch image
FROM scratch
WORKDIR /

# Copy the static binary
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/twitch-1337 /twitch-1337

# Create /data directory for persistence
# Note: In FROM scratch, we can't use RUN commands, so the app creates this at runtime
# Users must mount a volume at /data for persistence (e.g., -v ./data:/data)

# Run the bot
ENTRYPOINT ["/twitch-1337"]
