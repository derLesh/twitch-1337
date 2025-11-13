# Justfile for twitch-1337

# Default recipe - show available commands
default:
    @just --list

# Build the Docker image
build:
    docker build -t chronophylos/twitch-1337:latest .

# Build with no cache (force full rebuild)
build-no-cache:
    docker build --no-cache -t chronophylos/twitch-1337:latest .

# Run the container (requires .env file)
run:
    @mkdir -p data
    docker run -d \
        --name twitch-1337 \
        --env-file .env \
        --restart unless-stopped \
        chronophylos/twitch-1337:latest

# Run the container interactively (for testing)
run-test:
    @mkdir -p data
    docker run --rm \
        --name twitch-1337-test \
        --env-file .env \
        chronophylos/twitch-1337:latest

# Stop the container
stop:
    docker stop twitch-1337

# Remove the container
rm:
    docker rm twitch-1337

# Stop and remove the container
clean: stop rm

# View container logs (follow mode)
logs:
    docker logs -f twitch-1337

# View last 50 lines of logs
logs-tail:
    docker logs --tail 50 twitch-1337

# Restart the container
restart: stop
    docker start twitch-1337

# Build and run
up: build run

# Stop, remove, build, and run (fresh start)
reload: clean build run

# Push image to Docker Hub
push:
    docker push chronophylos/twitch-1337:latest

# Pull image from Docker Hub
pull:
    docker pull chronophylos/twitch-1337:latest

# Build local Rust binary (without Docker)
build-local:
    cargo build --release

# Build local musl binary (without Docker)
build-musl:
    cargo build --release --target x86_64-unknown-linux-musl

# Run local binary (without Docker)
run-local:
    cargo run --release

# Check code with clippy
lint:
    cargo clippy

# Format code
fmt:
    cargo fmt

# Check if code needs formatting
fmt-check:
    cargo fmt --check
