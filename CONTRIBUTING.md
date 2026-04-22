# Contributing

Thanks for contributing to twitch-1337. This file covers repo conventions that aren't obvious from the code. For build/test/CI details see [CLAUDE.md](CLAUDE.md).

## PR flow

1. Branch from `main`, commit, push.
2. `gh pr create` — fill in summary + test plan.
3. Wait for the 7 required status checks to go green (see CLAUDE.md § CI & branch policy).
4. Rebase on `main` if the `strict` check blocks merge.
5. `gh pr merge --squash` once reviews are in.

`main` is branch-protected: direct pushes are rejected, linear history is enforced, and all conversations must be resolved before merge.

## Labels

Labels use namespaced prefixes. Every issue and PR should carry at least one `type:` label and, when applicable, one or more `system:` labels. `topic:`, `deps:`, and `status:` stack on top as needed.

### `type:` — what kind of change

| Label | Use when |
|---|---|
| `type:bug` | Functional defect — something is broken |
| `type:feat` | New feature or capability |
| `type:refactor` | Structural refactor or code-quality cleanup (no user-visible change) |
| `type:security` | Security vulnerability or hardening (supersedes `type:bug`) |
| `type:docs` | Docs-only change |

Pick exactly one `type:` per issue. `type:security` takes precedence over `type:bug` when both apply.

### `system:` — which subsystem

| Label | Scope |
|---|---|
| `system:ai` | LLM, `!ai` command, memory, prefill |
| `system:aviation` | ADS-B, flight tracker, `!up`/`!fl`/`!track` |
| `system:ping` | Ping subsystem — `!p`, ping templates, render/validate |
| `system:tracker` | 1337 tracker + leaderboard (`!lb`) |
| `system:schedule` | Scheduled messages + hot-reload |
| `system:irc` | IRC client core, broadcast channel, latency monitor |
| `system:config` | `config.toml` schema, loader, file watcher |

Stack multiple when a change crosses subsystems.

### `topic:` — cross-cutting concern

| Label | Use when |
|---|---|
| `topic:concurrency` | Race conditions, locking, async correctness |
| `topic:reliability` | Error handling, fallbacks, timeouts, retries |
| `topic:perf` | Performance, latency, throughput |

### `deps:` — Dependabot buckets

| Label | Ecosystem |
|---|---|
| `deps:rust` | Cargo dependency update |
| `deps:actions` | GitHub Actions update |
| `deps:docker` | Docker base image update |

Applied automatically by Dependabot per `.github/dependabot.yml`.

### `status:` — triage state

| Label | Meaning |
|---|---|
| `status:wontfix` | Intentionally not pursuing |
| `status:duplicate` | Duplicate of another issue or PR |

## Commit messages

Conventional Commits. Scope is optional but helpful — prefer the subsystem (`ping`, `schedule`, `aviation`, `ai`, `config`, `ci`, etc.).

```
feat(ping): add !p pause command
fix(schedule): reconcile hot-reload on content changes
refactor: split main.rs into handler modules
```

Subject ≤ 50 chars. Body explains the *why* when it isn't obvious from the diff.

## Before committing

Run the pre-commit gate locally (CI will reject anything that fails):

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo audit` runs in CI; optional locally.
