# CI/CD Workflows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three GitHub Actions workflows that (1) run cargo checks on every commit and PR, (2) build and publish a Docker image to GHCR on every merge to `main`, and (3) open a weekly PR that refreshes the three public-dataset CSVs in `data/`.

**Architecture:** Three independent workflow files in `.github/workflows/`, each triggered differently and communicating only through the PR mechanism (the data-refresh workflow's PR runs through `ci.yml` automatically). The existing local-to-homelab deploy flow (`just deploy`) is untouched.

**Tech Stack:** GitHub Actions · `dtolnay/rust-toolchain` · `Swatinem/rust-cache` · `docker/build-push-action` · GHCR · `astral-sh/setup-uv` · `peter-evans/create-pull-request`.

**Spec:** `docs/superpowers/specs/2026-04-18-ci-cd-workflows-design.md`

---

## File structure

| Path | Purpose | Status |
|------|---------|--------|
| `.github/workflows/ci.yml` | Cargo fmt/clippy/test on PRs and pushes | Create |
| `.github/workflows/docker.yml` | Build and push image to GHCR on main push | Create |
| `.github/workflows/data-refresh.yml` | Weekly CSV regeneration + PR | Create |

No existing files are modified.

---

## Validation approach

GitHub Actions workflows can't be unit-tested the way code can. For each file this plan uses **`yamllint`** (installed locally at `/usr/bin/yamllint`) as the pre-push syntax check. The real validation is watching the workflow run on GitHub after push; a short post-implementation verification section at the end of this plan covers that.

If `actionlint` becomes available later (stricter schema validation for GitHub Actions specifically), it's a drop-in upgrade — same command shape.

---

## Task 1: Cargo checks workflow (`ci.yml`)

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Create the workflows directory**

Run: `mkdir -p .github/workflows`

- [ ] **Step 2: Write `ci.yml`**

```yaml
name: CI

on:
  pull_request:
    branches: [main]
  push:

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

jobs:
  check:
    name: fmt + clippy + test
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy

      - uses: Swatinem/rust-cache@v2

      - name: cargo fmt
        run: cargo fmt --all -- --check

      - name: cargo clippy
        run: cargo clippy --all-targets -- -D warnings

      - name: cargo test
        run: cargo test
```

Notes:
- `concurrency` block cancels superseded runs on the same ref, saving minutes on rapid pushes.
- `push` has no branch filter so straight-to-main pushes (which this repo uses) are also checked.

- [ ] **Step 3: Validate YAML syntax**

Run: `yamllint -d '{extends: relaxed, rules: {line-length: disable}}' .github/workflows/ci.yml`
Expected: no output (exit code 0).

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add cargo fmt, clippy, and test workflow"
```

---

## Task 2: Docker image build workflow (`docker.yml`)

**Files:**
- Create: `.github/workflows/docker.yml`

- [ ] **Step 1: Write `docker.yml`**

```yaml
name: Docker

on:
  push:
    branches: [main]

concurrency:
  group: docker-main
  cancel-in-progress: false

jobs:
  build:
    name: build and push
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write
    steps:
      - uses: actions/checkout@v4

      - uses: docker/setup-buildx-action@v3

      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - id: meta
        uses: docker/metadata-action@v5
        with:
          images: ghcr.io/chronophylos/twitch-1337
          tags: |
            type=raw,value=latest
            type=sha,prefix=sha-,format=short

      - uses: docker/build-push-action@v6
        with:
          context: .
          push: true
          tags: ${{ steps.meta.outputs.tags }}
          labels: ${{ steps.meta.outputs.labels }}
          cache-from: type=gha
          cache-to: type=gha,mode=max
```

Notes:
- `cancel-in-progress: false` — back-to-back merges both publish; we never want to silently skip an image build for a landed commit.
- Image name is lowercase (`chronophylos`); GHCR requires lowercase owner/repo.
- The short SHA tag provides a stable reference even after `latest` moves.

- [ ] **Step 2: Validate YAML syntax**

Run: `yamllint -d '{extends: relaxed, rules: {line-length: disable}}' .github/workflows/docker.yml`
Expected: no output.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/docker.yml
git commit -m "ci: add docker image build and GHCR publish on main"
```

---

## Task 3: Data refresh workflow (`data-refresh.yml`)

**Files:**
- Create: `.github/workflows/data-refresh.yml`

**Important constraint:** `peter-evans/create-pull-request` opens PRs using the token you pass it. If you pass the default `GITHUB_TOKEN`, GitHub will not trigger further workflows (including `ci.yml`) from that PR — this is a hard-coded safety measure against recursion. The spec explicitly wants `ci.yml` to run on the refresh PR, so the workflow uses a Personal Access Token supplied as a repo secret named `DATA_REFRESH_PAT`. Provisioning that PAT is documented at the bottom of this task.

- [ ] **Step 1: Write `data-refresh.yml`**

```yaml
name: Data refresh

on:
  schedule:
    - cron: '0 3 * * 0'  # Sundays 03:00 UTC
  workflow_dispatch:

jobs:
  refresh:
    name: regenerate CSVs and open PR
    runs-on: ubuntu-latest
    permissions:
      contents: write
      pull-requests: write
    steps:
      - uses: actions/checkout@v4
        with:
          token: ${{ secrets.DATA_REFRESH_PAT }}

      - uses: astral-sh/setup-uv@v5

      - name: Regenerate airlines.csv
        run: bash scripts/generate_airlines_csv.sh

      - name: Regenerate airports.csv
        run: uv run scripts/update-airports.py

      - name: Regenerate plz.csv
        run: uv run scripts/update-plz.py

      - name: Count rows for PR body
        id: counts
        run: |
          {
            echo "airlines=$(wc -l < data/airlines.csv)"
            echo "airports=$(wc -l < data/airports.csv)"
            echo "plz=$(wc -l < data/plz.csv)"
          } >> "$GITHUB_OUTPUT"

      - uses: peter-evans/create-pull-request@v7
        with:
          token: ${{ secrets.DATA_REFRESH_PAT }}
          branch: chore/data-refresh
          title: 'chore(data): refresh CSVs'
          commit-message: 'chore(data): refresh CSVs'
          body: |
            Automated weekly refresh of public-dataset CSVs.

            **Row counts**
            - `data/airlines.csv`: ${{ steps.counts.outputs.airlines }} rows
            - `data/airports.csv`: ${{ steps.counts.outputs.airports }} rows
            - `data/plz.csv`: ${{ steps.counts.outputs.plz }} rows

            Merge if the diff looks reasonable. If CI fails, a parser likely
            needs to be adjusted for an upstream schema change.
          delete-branch: true
```

Notes:
- Reusing `branch: chore/data-refresh` means weekly re-runs *update* the existing PR rather than opening duplicates.
- `delete-branch: true` cleans up after merge.
- If the working tree is clean after the scripts run, `create-pull-request` is a no-op — no empty PRs.

- [ ] **Step 2: Validate YAML syntax**

Run: `yamllint -d '{extends: relaxed, rules: {line-length: disable}}' .github/workflows/data-refresh.yml`
Expected: no output.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/data-refresh.yml
git commit -m "ci: add weekly data-refresh workflow opening PR for CSV updates"
```

- [ ] **Step 4 (manual, user-side): Provision `DATA_REFRESH_PAT`**

The workflow won't run end-to-end until this secret exists in the repo. Provide these instructions to the user (or perform them yourself if you have access):

1. Visit <https://github.com/settings/personal-access-tokens/new>
2. Create a **fine-grained PAT** scoped to the `twitch-1337` repository only.
3. Permissions: `Contents: Read and write` and `Pull requests: Read and write`.
4. Expiration: your choice (up to 1 year; set a calendar reminder if you want continuous operation).
5. Copy the token, then in the repo go to **Settings → Secrets and variables → Actions → New repository secret**.
6. Name: `DATA_REFRESH_PAT`. Value: the token.

Until this is done, the scheduled workflow will fail at the checkout step. Manual runs via `workflow_dispatch` can still be used to verify the script portion works — add a temporary fallback `token: ${{ github.token }}` to the checkout step for local testing, but revert before merging.

---

## Post-implementation verification

After all three workflows are committed and pushed to `main`:

- [ ] **Verify `ci.yml`** — the push itself triggers the workflow. Watch the Actions tab for a green run on the fmt/clippy/test job.

- [ ] **Verify `docker.yml`** — same push triggers it. Expected: a new package under <https://github.com/Chronophylos?tab=packages> named `twitch-1337` with tags `latest` and `sha-<short>`. First build won't have GHA cache so it takes several minutes; subsequent builds warm.

- [ ] **Verify `data-refresh.yml`** — after provisioning `DATA_REFRESH_PAT`, manually trigger via Actions → Data refresh → Run workflow. Expected outcome: either a no-op (data unchanged) or a new PR titled `chore(data): refresh CSVs`. That PR should in turn trigger a `ci.yml` run.

If `docker.yml` fails on first run with a `denied: permission_denied` error when pushing to GHCR, that's usually a GitHub Actions package-permission setting: go to the repo's **Settings → Actions → General → Workflow permissions** and confirm "Read and write permissions" is selected (or alternatively leave it at read-only and the per-workflow `permissions: packages: write` handles it — which is the configuration in this plan).

---

## Self-review checklist

- **Spec coverage:**
  - `ci.yml` (cargo checks) → Task 1 ✓
  - `docker.yml` (GHCR image build on main) → Task 2 ✓
  - `data-refresh.yml` (weekly CSV PR) → Task 3 ✓
  - Interaction model (data-refresh PR triggers ci.yml) → Task 3 step 4 (PAT requirement) ✓
  - Caching strategy (rust-cache + gha buildx cache) → Task 1 + Task 2 ✓
  - Out-of-scope items → respected (no auto-merge, no homelab deploy changes, no semver tags, no security scanning) ✓

- **Placeholder scan:** none — every step has concrete commands or code.
- **Type consistency:** secret name `DATA_REFRESH_PAT` used consistently; image name `ghcr.io/chronophylos/twitch-1337` used consistently; branch name `chore/data-refresh` used consistently.
