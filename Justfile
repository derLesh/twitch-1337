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
