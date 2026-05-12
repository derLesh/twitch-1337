# Justfile for twitch-1337

# Default recipe - show available commands
default:
  @just --list

# Build the Docker image. Passes the current commit SHA so the running
# binary can report it via `/version` and the sidebar brand-meta.
build:
  podman build --build-arg GIT_SHA=$(git rev-parse --short=12 HEAD) -t chronophylos/twitch-1337:latest .

# Build with no cache (force full rebuild)
build-no-cache:
  podman build --no-cache --build-arg GIT_SHA=$(git rev-parse --short=12 HEAD) -t chronophylos/twitch-1337:latest .

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

# Run the dashboard alone (no IRC, no Helix) on a non-conflicting port.
# Lets a worktree iterate on /pings + /memory views in parallel with the
# full bot above — both share dev-data via atomic tmp+rename, so writes
# from either side land cleanly (last-writer-wins, never corrupted).
# Default bind 127.0.0.1:8761; pass `bind=...` to override.
dev-web bind="127.0.0.1:8761":
  DATA_DIR=./dev-data BIND_ADDR={{bind}} RUST_LOG=info,twitch_1337_web=debug cargo run -p twitch-1337-web --bin web-dev --features dev-login

# Rebuild + restart dev-web on Rust/template changes. Requires
# `cargo install cargo-watch`. Browser auto-refreshes via livereload
# when the new server comes up.
dev-web-watch bind="127.0.0.1:8761":
  DATA_DIR=./dev-data BIND_ADDR={{bind}} RUST_LOG=info,twitch_1337_web=debug \
    cargo watch -w crates/web/src -w crates/web/templates -w crates/core/src \
      -x 'run -p twitch-1337-web --bin web-dev --features dev-login'

# Kill the running web-dev binary (matches the compiled path so it
# doesn't escape to the parent shell or to a cargo invocation elsewhere).
dev-web-stop:
  -pkill -f 'target/debug/web-dev' 2>/dev/null

# One-shot: mint a dev session at /_dev/login + screenshot a path to PNG.
# Requires `dev-web` already running on `base`. Reuses /tmp/wd-cprof so
# the second+ shot skips the auth round-trip. Defaults: /pings →
# /tmp/wd.png. Override `path=`, `file=`, `base=` as needed.
dev-web-shot path="/pings" file="/tmp/wd.png" base="http://127.0.0.1:8761":
  @rm -rf /tmp/wd-cprof
  @chromium --headless=new --no-sandbox --disable-gpu --hide-scrollbars --window-size=1400,900 --user-data-dir=/tmp/wd-cprof --virtual-time-budget=4000 --screenshot={{file}} "{{base}}/_dev/login?next={{path}}" 2>&1 | tail -1
  @echo "shot {{base}}{{path}} -> {{file}}"

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

