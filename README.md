# knack

[![Crates.io](https://img.shields.io/crates/v/knack.svg)](https://crates.io/crates/knack)
[![Docs.rs](https://docs.rs/knack-core/badge.svg)](https://docs.rs/knack-core)
[![CI](https://github.com/ajac-zero/knack/actions/workflows/ci.yml/badge.svg)](https://github.com/ajac-zero/knack/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/knack.svg)](https://github.com/ajac-zero/knack/blob/main/LICENSE)

`knack` is an open-source Rust CLI and self-hostable registry project for teams to package, validate, version, publish, discover, install, and govern Agent Skills.

The current v0 focuses on local skill authoring and distribution primitives. See [`PROJECT.md`](PROJECT.md) for the north-star product direction.

## Install

Install the CLI from crates.io:

```bash
cargo install knack
```

Install the registry server:

```bash
cargo install knack-registry
```

Or build both from source:

```bash
git clone https://github.com/ajac-zero/knack
cd knack
cargo build --release
# Binaries land at target/release/knack and target/release/knack-registry
```

Verify the install:

```bash
knack --help
```

## Usage

Create a skill:

```bash
knack new rust-code-review
```

Initialize a project manifest:

```bash
knack init
```

This seeds the public knack registry (`public` → `https://knack.ajac-zero.com`) into the new manifest AND sets it as the default so you can `knack find <term>` and `knack add <namespace>/<name>` immediately without needing to type the `public:` prefix. Pass `--no-public-registry` to skip if you want a bare manifest or only intend to use an internal registry. Either way, you can remove the bootstrapped entry later with `knack registry remove public` (and drop `default_registry` from `[install]` or point it at a different alias).

Install commands accept three shapes:

- `knack add anthropics/pdf` — bare, resolved via the manifest's `install.default_registry`. Shortest form; recommended for interactive use.
- `knack add public:anthropics/pdf` — explicit registry prefix. Works regardless of default config; use in scripts and shared docs where you don't want to depend on the reader's config.
- `knack add gh:anthropics/skills/skills/pdf` — direct GitHub install, bypasses any registry.

If no `default_registry` is set but exactly one registry is configured, that registry becomes the implicit default automatically. Multi-registry setups with no explicit default require the fully-qualified form.

By default, commands use project scope:

```text
.agents/knack.toml
.agents/knack.lock
.agents/skills/
```

Pass `-g` (or `--global`) on any command to operate on user-wide skills instead of the current project. `knack add` creates the manifest on first use, so an explicit `knack init` is only needed if you want to customize the install target:

```bash
knack add gh:anthropics/skills/skills/pdf -g
knack sync -g
knack list -g
```

Global scope uses:

```text
~/.agents/knack.toml
~/.agents/knack.lock
~/.agents/skills/
```

System-wide defaults are configured at:

```text
/etc/knack/knack.toml
/usr/local/share/knack/skills/
```

These have no dedicated CLI flag because they are an administrator concern. Edit `/etc/knack/knack.toml` directly (with `sudo`) to seed registry aliases that every user on the machine should inherit.

Registry aliases are inherited in this order, with later layers overriding earlier layers:

```text
system (/etc/knack/knack.toml)
global (~/.agents/knack.toml)
project (./.agents/knack.toml)
```

That lets administrators inject aliases such as `tea:` for all users while still allowing user or project-level overrides.

Read-only commands (`knack find`, `knack registry list`) search the merged set of registries from all three layers automatically. They work from a directory with no project manifest, so a globally-registered `company:` alias is reachable without an explicit `-g`.

`knack find <query>` ranks results best-match-first: a hit in a skill's name or tags outranks one that only appears incidentally in its description, and terms that are common across the registry (e.g. "deploy") are discounted relative to terms that are rare and therefore more discriminating, so a query for a generic word doesn't surface every skill that happens to mention it once in passing. Each result shows its description so you can tell why it matched without installing it first. Output is capped at 10 matches by default — pass `--limit N` to see more. If one of your configured registries is unreachable, `find` warns and keeps going with the rest rather than failing the whole command.

Add and install a skill source into the manifest target:

```bash
knack add gh:anthropics/skills/skills/pdf
```

This updates `.agents/knack.toml` and writes `.agents/knack.lock` with the resolved source and a deterministic checksum of the installed skill contents.

Install all skills declared in `.agents/knack.toml` (uses the lockfile,
reproducible):

```bash
knack sync
```

For CI: verify the install and lockfile are consistent with the
manifest without modifying anything. Exits non-zero with an actionable
message on missing lockfile entries, missing installs, or checksum
drift:

```bash
knack sync --check
```

Pull upstream changes for skills tracking a moving ref (branch, tag).
Sources pinned to a SHA-shaped ref are skipped — pass `-f` / `--force`
to retry them anyway. Pass `-n` / `--dry-run` to preview what would
change without touching the install dir or lockfile (still fetches
from the network so the preview is accurate):

```bash
knack update              # branch/tag-tracking skills get refreshed
knack update --dry-run    # report what would change; modify nothing
knack update --force      # ignore pinning, re-fetch everything
```

The lockfile records each skill's `resolved` source as a
content-addressed commit SHA — `gh:` and `git+` URLs embed `@<sha>`,
`http+knack:` URLs append `#sha=<sha>`. That makes `knack sync` truly
reproducible: a teammate cloning the repo six months later installs
the same commits, regardless of where `main` has moved since.

Validate a skill:

```bash
knack validate rust-code-review
```

Preview a skill before installing it. `inspect` resolves local paths,
archives, Git sources, and configured registry aliases without modifying the
manifest, lockfile, or install directory:

```bash
knack inspect public:anthropics/pdf
knack inspect gh:anthropics/skills/skills/pdf
```

The preview shows the skill metadata, resolved source and commit when
available, deterministic checksum, and included files.

Package a skill:

```bash
knack pack rust-code-review --output dist
```

Install a skill directory or package archive:

```bash
knack install rust-code-review --target .agents/skills
knack install dist/rust-code-review.skill.tar.gz --target .agents/skills
```

Install directly from a public GitHub repository:

```bash
knack install gh:owner/repo/path/to/skill --target .agents/skills
knack install gh:owner/repo@ref/path/to/skill --target .agents/skills
```

For example:

```bash
knack install gh:anthropics/skills/skills/pdf --target .agents/skills
```

Install from any Git host supported by your local `git` command:

```bash
knack install git+https://git.example.com/org/skills.git//path/to/skill
knack install git+ssh://git@git.example.com/org/skills.git@main//path/to/skill
```

Register a Git host alias in `knack.toml`:

```bash
knack registry add git+ssh://git@gitea.example.com tea
knack registry list
```

For HTTP knack registries the alias can be omitted — the CLI fetches
`GET <url>/info` and adopts whatever name the registry advertises so
every client of the same registry uses the same alias:

```bash
knack registry add http://127.0.0.1:7349           # adopts the advertised name
knack registry add http://127.0.0.1:7349 myalias   # explicit override
```

Then add skills through the alias:

```bash
knack add tea:platform/agent-skills/rust-code-review
knack add tea:platform/agent-skills@v1.2.0/rust-code-review
```

Alias syntax is:

```text
alias:owner/repo[@ref]/path/to/skill
```

Generate a searchable registry index from a local tree of skills:

```bash
knack index generate ./skills \
  --source-prefix gh:owner/repo/skills \
  --output knack.index.toml
```

Serve the index with `knack-registry`:

```bash
knack-registry --index knack.index.toml --bind 127.0.0.1:7349
```

Build and run the registry container:

```bash
docker build -t knack-registry .
docker run --rm -p 7349:7349 \
  -v "$PWD:/data:ro" \
  knack-registry
```

The image defaults to:

```bash
knack-registry \
  --index /data/knack.index.toml \
  --skills-root /data/skills \
  --name company \
  --bind 0.0.0.0:7349
```

Override arguments as needed:

```bash
docker run --rm -p 7349:7349 \
  -v "$PWD:/data:ro" \
  knack-registry \
  --index /data/knack.index.toml \
  --skills-root /data/skills \
  --name platform \
  --bind 0.0.0.0:7349
```

To make the HTTP registry the only thing users need to interact with, serve skill archives too:

```bash
knack-registry \
  --index knack.index.toml \
  --skills-root ./skills \
  --name company \
  --bind 127.0.0.1:7349
```

With `--name company`, search results return proxy install sources such as `company:deploy-container`. The CLI resolves those through the HTTP registry and downloads the skill archive from the registry server. The registry also advertises the name at `GET /info` so clients can `knack registry add <url>` without supplying an alias.

When a skill's backing source is a git repository, the registry includes the resolved commit SHA in the archive response via the `X-Knack-Resolved-Sha` header. The CLI captures it into the lockfile so HTTP-registry installs are pinned the same way `gh:`/`git+` installs are.

Register and search that registry from the CLI:

```bash
knack registry add http://127.0.0.1:7349 local
knack find pdf
```

Each match renders as a compact card: a header that's simultaneously the skill's identity and the exact thing to paste after `knack add`, plus its description. The `knack add` prefix is spelled out once in the heading rather than repeated on every card — every card's header already is the rest of the command:

```text
1 skill found — install with `knack add <name>`

owner/pdf
  Work with PDF files...
```

In proxy mode, results look the same way:

```text
1 skill found — install with `knack add <name>`

deploy-container
  Deploy containers to Kubernetes.
```

With more than one registry configured, each header gains a `<registry>:` prefix so you can tell where a result came from and install it unambiguously — regardless of which registry happens to be your configured default, since the explicit `<registry>:<name>` form always resolves correctly:

```text
2 skills found — install with `knack add <name>`

acme-internal:acme-platform-team/deploy-app
  Deploy an app via Argo CD.

public-mirror:acme-platform-team/release-pr
  Mirror of release-pr on the public mirror.
```

Users can install without knowing the backing Git repo:

```bash
knack add company:deploy-container
```

For skills scattered across one or more Git repositories, prefer dynamic source entries. The registry fetches the backing source, scans for `SKILL.md`, and derives each skill's name and description from the skill itself:

```toml
[[source]]
source = "tea:platform/agent-skills"
tags = ["deploy", "kubernetes"]
```

At startup, that materializes skills such as `tea:platform/agent-skills/deploy-container` from `platform/agent-skills/deploy-container/SKILL.md`. This avoids duplicating fragile metadata in `knack.index.toml`.

Dynamic sources must refresh successfully on startup before the registry serves traffic. After startup, the registry refreshes dynamic sources every 300 seconds by default and keeps serving the last good index if a later refresh fails. Disable background refresh with `--refresh-interval-seconds 0` or tune it with another interval.

Pass `--cache-dir <path>` to keep the cloned backing repos across restarts. When set, refreshes do `git fetch + git reset --hard` against the existing clone instead of re-cloning, and archive requests read straight from the cache directory — no per-request git operations. Point it at a persistent volume (Fly.io Volume, EFS mount, GCP Cloud Run volume, anything that survives container restart) for the full benefit. When omitted, the registry uses a per-process tempdir that's rebuilt on every cold start — that's the right shape for ephemeral-filesystem platforms like Cloudflare Containers.

### Static deployment (Cloudflare R2 + Worker)

For a public, read-mostly registry, the cheapest shape is to skip running a live registry process entirely. Run `knack-registry build-static` from a CI cron job, upload the output to an object store (R2, S3, GCS, anything), and serve it via a small edge worker:

```bash
knack-registry build-static \
    --index registries/public.toml \
    --output ./dist \
    --name public
```

This produces `info.json`, `index.json`, `sha-map.json`, and one `skills/<name>.skill.tar.gz` per indexed skill. The output is everything a knack CLI client needs; an edge function in front of the bucket maps the four CLI endpoints (`/info`, `/index`, `/search`, `/skills/<name>/archive`) onto these files. See [`examples/cloudflare-worker/`](examples/cloudflare-worker/) for a working Worker + R2 setup with a daily GitHub Actions cron, free-tier-friendly at the scale of the public registry (~200 skills, single-digit thousands of requests/day).

Tradeoff: refresh granularity drops from `--refresh-interval-seconds` (default 300s) to whatever your cron interval is (daily for the public registry). Static loses dynamic queries, auth, and the ability to index private sources — keep `knack-registry serve` for internal team registries where any of those matter.

Static entries are still supported for hand-curated overrides:

```toml
[[skill]]
name = "deploy-container"
description = "Deploy containers into the internal Kubernetes cluster."
source = "tea:payments/api/.agents/skills/deploy-container"
tags = ["deploy", "kubernetes"]
```

Start the registry with a source alias so it can fetch those backing sources server-side:

```bash
knack-registry \
  --index knack.index.toml \
  --name company \
  --source-alias tea=git+ssh://git@gitea.example.com \
  --refresh-interval-seconds 300 \
  --bind 127.0.0.1:7349
```

Users still only need the HTTP registry:

```bash
knack registry add http://127.0.0.1:7349       # adopts the advertised name `company`
knack find deploy
knack add company:deploy-container
```

`gh:` sources resolve directly against github.com and need no `--source-alias`. The curated source list used by the project's public registry instance lives at [`registries/public.toml`](registries/public.toml) — mirror it for your own registry, or open a PR with a new `[[source]]` entry to propose adding a source to the public one. The file's header documents the curation criteria and PR process.

Publish a local skill into a git-backed team skills repository:

```bash
knack registry add git+ssh://git@gitea.example.com tea
knack publish ./my-skill \
  --registry tea \
  --repo platform/agent-skills
```

Publishing currently supports `git-host` registries. It clones the target repository, copies the skill into `skills/<skill-name>`, regenerates `knack.index.toml`, commits the change, and pushes it. Use `--no-push` to leave the commit local in the temporary checkout for debugging.

After the registry server is serving the updated `knack.index.toml`, teammates can discover and install the skill:

```bash
knack find my-skill
knack add tea:platform/agent-skills/skills/my-skill
```

List installed skills:

```bash
knack list --target .agents/skills
```

## v0 Scope

Implemented:

- Skill scaffolding.
- Agent Skills frontmatter validation.
- Deterministic `.skill.tar.gz` packaging.
- Local installation from directories and package archives.
- GitHub installation with `gh:owner/repo[@ref]/path/to/skill`.
- Generic Git installation with `git+<url>[@ref]//path/to/skill`.
- Git-host registry aliases with `alias:owner/repo[@ref]/path/to/skill`.
- Searchable HTTP registries served by `knack-registry`.
- Proxied HTTP registry installs with `registry:skill-name`.
- Registry-side proxying from indexed Git backing sources.
- Registry index generation from local skill directories.
- Publishing skills to git-backed team repositories.
- Project manifests with `knack.toml`.
- Lockfiles with `knack.lock`.
- Project and global scoped config/install paths.
- System scoped config at `/etc/knack/knack.toml`.
- Layered registry alias inheritance from system to global to project.
- `add`, `sync`, and `update` workflows for reproducible project installs.
- Read-only skill inspection before installation.
- Content-addressed lockfile entries (commit SHA captured for `gh:`,
  `git+`, and `http+knack:` sources).
- `knack sync --check` for CI: assert install + lockfile consistency.
- `knack update --dry-run` for previewing upstream changes.
- Versioned lockfile schema; future-incompatible lockfiles refuse to
  load rather than silently round-tripping.
- Listing installed skills.

Not implemented yet:

- Signing, provenance, and policy checks.
