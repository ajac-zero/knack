# AGENTS.md

Context for agents working on `knack` — a Rust CLI + self-hostable HTTP
registry for Agent Skills.

## Read first

- `README.md` — current user-facing surface (commands, scopes, registry modes).
- `PROJECT.md` — north-star direction, kept separate from `README.md` on purpose.

## Workspace shape

Three crates (Cargo workspace, edition 2024, `resolver = "3"`):

- `knack-cli/` — produces the **`knack`** binary (note: binary name ≠ crate name).
- `knack-registry/` — produces the `knack-registry` binary.
- `knack-core/` — shared library; **the only crate with unit tests** (4 in `src/lib.rs`).

Async/sync split: **CLI uses `reqwest` blocking**; **registry uses `tokio`/`axum` async**. Don't pull tokio into the CLI or blocking I/O into the registry.

## Commands

```bash
cargo build --workspace                # full check
cargo test -p knack-core               # the only tests that exist
cargo run -p knack-cli -- --help       # exercise the CLI
cargo run -p knack-registry -- --help  # exercise the registry
```

No CI, no `rustfmt.toml`, no `clippy.toml`. Defaults apply, none of it is enforced anywhere except locally. Run `cargo fmt` / `cargo clippy --workspace` before committing meaningful changes.

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

**Config scope layering** (`knack-cli/src/main.rs`, `enum Scope`): registries from `system` (`/etc/knack/knack.toml`) and `global` (`~/.config/knack/knack.toml`) are merged into the effective set used by `project` (`.agents/knack.toml`). Later layers override earlier ones. See `effective_registries()`.

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

The `repository` field was deliberately removed from all `Cargo.toml`s during the rename, and the `registry = "gitea"` marker was dropped from the `knack-core` dependency in `knack-cli/Cargo.toml`. Both must be re-added before publishing to any registry. The previous gitea setup is documented in commit `tyrrtzzq build: make crates publishable to gitea`.

## Things to leave alone

- `.opencode/` — gitignored; belongs to an unrelated agent-skills plugin (see `.opencode/opencode.json`). Don't try to influence this repo's behavior by editing it.
- `Cargo.lock` — committed, expected. Don't delete.
