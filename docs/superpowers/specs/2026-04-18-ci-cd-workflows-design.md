# CI/CD Workflows — Design

## Goal

Introduce automated CI/CD via GitHub Actions to:

1. Run cargo checks on every commit and pull request.
2. Build and publish a Docker image on every merge to `main`.
3. Regenerate the public-dataset CSVs in `data/` on a weekly schedule and propose them via pull request.

No existing CI is in place; the repository is hosted on GitHub (`github.com:Chronophylos/twitch-1337.git`).

## Non-goals

- Deploying the built image to the homelab. The existing `just deploy` flow (local podman build + SSH push) remains untouched.
- Pinning a minimum supported Rust version (MSRV). CI uses stable Rust, matching local development.
- Release tagging or semver tag propagation to image tags. Only `latest` and `sha-<short>` are published.

## Workflows

Three workflows under `.github/workflows/`. They share nothing operationally but interact through the PR trigger: the data-refresh workflow opens a PR, which then runs `ci.yml` automatically.

### `ci.yml` — cargo checks

**Triggers**
- `pull_request` targeting `main`
- `push` to any branch (catches straight-to-main pushes, which the repo currently uses)

**Runner**: `ubuntu-latest`. Single job, steps run sequentially to share the cargo cache.

**Steps**
1. `actions/checkout@v4`
2. `dtolnay/rust-toolchain@stable` with components `rustfmt, clippy`
3. `Swatinem/rust-cache@v2` — caches `~/.cargo` and `target/` keyed on `Cargo.lock`
4. `cargo fmt --all -- --check`
5. `cargo clippy --all-targets -- -D warnings`
6. `cargo test`

**Permissions**: default read-only.

**Rationale for single sequential job**: the project is small enough that cold-cache parallelism wouldn't beat shared-cache sequential. A warm run completes in a few minutes; splitting into matrix jobs would invalidate caches across them.

### `docker.yml` — image build on main

**Triggers**: `push` to `main`. Concurrency group keyed on `main` with `cancel-in-progress: false` so back-to-back merges both publish.

**Runner**: `ubuntu-latest`. Single job.

**Steps**
1. `actions/checkout@v4`
2. `docker/setup-buildx-action@v3`
3. `docker/login-action@v3` against `ghcr.io` using `${{ github.actor }}` and `${{ secrets.GITHUB_TOKEN }}`
4. `docker/metadata-action@v5` to compute tags
5. `docker/build-push-action@v6`
   - `context: .`, existing `Dockerfile`
   - `push: true`
   - `cache-from: type=gha`
   - `cache-to: type=gha,mode=max`

**Image name**: `ghcr.io/chronophylos/twitch-1337`.

**Tags published**
- `latest` (on every main push)
- `sha-<short-sha>` (traceable reference per commit)

**Permissions**: `contents: read`, `packages: write`.

**Registry**: GitHub Container Registry. GHCR auto-creates the package on first push; visibility defaults to inheriting the repo and is managed through GitHub package settings. No additional secrets required beyond `GITHUB_TOKEN`.

### `data-refresh.yml` — weekly CSV refresh

**Triggers**
- `schedule: cron "0 3 * * 0"` (Sundays at 03:00 UTC — low-traffic window for upstream sources)
- `workflow_dispatch` for manual runs

**Runner**: `ubuntu-latest`.

**Steps**
1. `actions/checkout@v4`
2. `astral-sh/setup-uv@v5` — the Python scripts already use PEP 723 inline `# /// script` metadata
3. Regenerate datasets:
   - `bash scripts/generate_airlines_csv.sh` → `data/airlines.csv`
   - `uv run scripts/update-airports.py` → `data/airports.csv`
   - `uv run scripts/update-plz.py` → `data/plz.csv`
4. `peter-evans/create-pull-request@v7`
   - `branch: chore/data-refresh` (stable branch name; weekly re-runs update the existing PR instead of opening duplicates)
   - `title: chore(data): refresh CSVs`
   - `body`: lists row counts per file; notes which files actually changed
   - `commit-message: chore(data): refresh CSVs`
   - No-op if the working tree is clean

**Permissions**: `contents: write`, `pull-requests: write`.

**Why PRs instead of direct commits**: upstream data providers occasionally reformat or drop large chunks. A PR run surfaces the diff for human review before it lands on `main`. The PR also triggers `ci.yml`, so `cargo test` validates that the parser still accepts the new data before merge.

## Interaction model

```
commit to any branch  ──►  ci.yml
open PR to main       ──►  ci.yml
merge / push to main  ──►  ci.yml + docker.yml
weekly cron           ──►  data-refresh.yml ──► PR ──► ci.yml ──► (human merges) ──► docker.yml
```

## Caching strategy

- **Cargo**: `Swatinem/rust-cache@v2` handles `~/.cargo/registry`, `~/.cargo/git`, and `target/`. Cache key derived from `Cargo.lock`. Separate cache per job; safe for the single-job `ci.yml`.
- **Docker**: GitHub Actions cache backend (`type=gha`) with `mode=max` so intermediate stages (cargo-chef planner, cacher, builder) are all cached. Expected warm-build time: ~30-60 s; cold-build time: several minutes.

## Out-of-scope follow-ups (explicitly deferred)

- Auto-merge for `chore/data-refresh` PRs after CI passes — could be added later via `gh pr merge --auto` in the workflow if the human review becomes redundant.
- Pulling the published GHCR image from the homelab instead of rebuilding locally with podman — a one-line change in `Justfile` that the user can make whenever they choose.
- Publishing semver tags alongside `latest` and `sha-*` — only relevant once the project adopts tagged releases.
- Security scanning (e.g. `cargo audit`, `trivy` on the image).
