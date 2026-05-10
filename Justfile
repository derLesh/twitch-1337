# Justfile for twitch-1337

# Default recipe - show available commands
default:
  @just --list

# Build the Docker image
build:
  podman build -t chronophylos/twitch-1337:latest .

# Build with no cache (force full rebuild)
build-no-cache:
  podman build --no-cache -t chronophylos/twitch-1337:latest .

# Push the image to docker host
push:
  podman save localhost/chronophylos/twitch-1337:latest | ssh docker.homelab 'docker load'

# Restart container on docker host
restart:
  ssh docker.homelab 'docker compose --ansi always --project-directory twitch up -d'

# Tail logs on docker host
logs:
  ssh docker.homelab 'docker compose --ansi always --project-directory twitch logs -f'

# Deploy image and restart pod
deploy: build push restart

# Mount prod data dir from docker host via sshfs at ./prod-data
mount-prod-data:
  mkdir -p prod-data
  sshfs root@docker.homelab:/opt/dockhand/stacks/twitch/data prod-data

# Unmount prod data dir
unmount-prod-data:
  fusermount -u prod-data

# Run bot + dashboard locally with debug logging and OAuth bypass.
# `dev-login` feature mounts /_dev/login on the dashboard — open it once to
# mint a mod session without round-tripping Twitch OAuth. Production builds
# (Dockerfile) compile without the feature, so the route does not exist.
dev:
  DATA_DIR=./dev-data RUST_LOG=info,twitch_1337=debug,twitch_1337_core=debug,twitch_1337_web=debug cargo run -p twitch-1337 --features dev-login

# Run tests with minimal output
test-brief:
    @cargo test --workspace --quiet 2>&1 | grep "test result" | awk ' \
    BEGIN { status = "ok" } \
    /FAILED/ { status = "FAILED" } \
    { \
        passed += $4; \
        failed += $6; \
        ignored += $8; \
        measured += $10; \
        filtered += $12; \
        gsub(/s/, "", $17); \
        time += $17; \
    } \
    END { \
        printf "test result: %s. %d passed; %d failed; %d ignored; %d measured; %d filtered out; finished in %.2fs\n", \
        status, passed, failed, ignored, measured, filtered, time \
    }'

