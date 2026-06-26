# AGENTS.md

Context for agents working on `knack` — a Rust CLI + self-hostable HTTP
registry for Agent Skills.

## Read first

- `README.md` — current user-facing surface (commands, scopes, registry modes).
- `PROJECT.md` — north-star direction, kept separate from `README.md` on purpose.

## Workspace shape

Three crates (Cargo workspace, edition 2024, `resolver = "3"`):

- `knack/` — produces the `knack` binary (this is the published CLI crate).
- `knack-registry/` — produces the `knack-registry` binary.
- `knack-core/` — shared library; **the only crate with unit tests** (4 in `src/lib.rs`).

Async/sync split: **CLI uses `reqwest` blocking**; **registry uses `tokio`/`axum` async**. Don't pull tokio into the CLI or blocking I/O into the registry.

## Commands

```bash
cargo build --workspace                # full check
cargo test -p knack-core               # the only tests that exist
cargo run -p knack -- --help           # exercise the CLI
cargo run -p knack-registry -- --help  # exercise the registry
```

CI runs `fmt`, `clippy --workspace --all-targets -- -D warnings`, and `test --workspace` on every push to `main` and every PR (`.github/workflows/ci.yml`). Run the same three locally before pushing or CI will go red. No `rustfmt.toml` or `clippy.toml` — defaults apply.

Releases are automated by `release-plz` (`.github/workflows/release-plz.yml`). Push Conventional Commits to `main`; release-plz opens a PR bumping versions and updating changelogs. Merging that PR publishes to crates.io in dependency order. Requires the `CARGO_REGISTRY_TOKEN` secret on the repo. Manual `cargo publish` is still possible but should not be needed.

## Recent rename: `skillhub` → `knack`

The project was renamed in commit `yvrusxko`. Things to know:

- The workspace directory is still `/home/coder/skillx` — intentionally not renamed.
- Commits before the rename (e.g. `chore: release skillhub cli 0.1.1`) and the bookmark `remember-remove-gitea-specifics` still say `skillhub`. **Do not rewrite that history.**
- If you find any live `skillhub` reference in tracked files, it's a bug — the rename was meant to be exhaustive.
- Any previously `cargo install`-ed `skillhub` binary on the system is stale; uninstall it separately.

## Version control: jj, not git

This repo is colocated jj + git (`.jj/` and `.git/` both present). Use **jj** commands locally. The `onevcat-jj` skill auto-loads on `.jj/` presence and is the source of truth.

Commit message convention (visible in `jj log`): Conventional Commits with scopes — `feat(cli):`, `fix(cli):`, `feat(registry):`, `build:`, `chore:`, `docs:`. Use `!` for breaking changes (e.g. `chore!: rename project...`).

## Architecture notes that aren't obvious from filenames

**Config scope layering** (`knack/src/main.rs`, `enum Scope`): registries from `system` (`/etc/knack/knack.toml`) and `global` (`~/.config/knack/knack.toml`) are merged into the effective set used by `project` (`.agents/knack.toml`). Later layers override earlier ones. See `effective_registries()`.

**Custom source URL schemes the CLI parses** (project vocabulary; not standards):

- `gh:owner/repo[@ref]/path` — GitHub tarball download
- `git+<url>[@ref]//path/to/skill` — generic git clone (note the `//` separator)
- `alias:owner/repo[@ref]/path` — git-host registry alias
- `alias:skill-name` — http registry alias (resolves to `http+knack:` internally)
- `http+knack:<url>` — internal proxied archive URL, **not user-facing**

**Frontmatter parsing** (`knack-core/src/lib.rs`, `SkillFrontmatter`) uses `#[serde(deny_unknown_fields)]`. Adding any new field to `SKILL.md` frontmatter requires adding it to the struct, or every existing skill fails to parse.

**Skill name == directory name** is enforced by `validate_skill()`. The same `validate_skill_name` is also used to validate index entries — change it carefully.

**Registry dynamic sources** (`knack-registry/src/main.rs::materialize_dynamic_sources`) clone backing git repos at startup and refresh every 300s by default. Startup fails if the initial materialize fails; subsequent failures keep serving the last good index.

## Publishing notes

The three crates are configured for crates.io. Shared metadata (`version`, `edition`, `rust-version`, `license`, `repository`, `homepage`, `authors`, `readme`) lives in `[workspace.package]` in the root `Cargo.toml`; each crate inherits via `field.workspace = true`. Per-crate fields (`description`, `keywords`, `categories`) stay in the crate's own `Cargo.toml`.

**Publish order is forced**: `knack-core` first, then `knack` and `knack-registry` (both depend on `knack-core` by both `version` and `path`).

`rust-version = "1.85"` is the MSRV (edition 2024 requirement). Bump it deliberately if you raise the floor.

The earlier gitea publishing setup (commit `tyrrtzzq build: make crates publishable to gitea`) was dropped during the rename and is not currently configured.

## Things to leave alone

- `.opencode/` — gitignored; belongs to an unrelated agent-skills plugin (see `.opencode/opencode.json`). Don't try to influence this repo's behavior by editing it.
- `Cargo.lock` — committed, expected. Don't delete.
