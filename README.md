# knack

[![Crates.io](https://img.shields.io/crates/v/knack.svg)](https://crates.io/crates/knack)
[![Docs.rs](https://docs.rs/knack-core/badge.svg)](https://docs.rs/knack-core)
[![CI](https://github.com/ajac-zero/knack/actions/workflows/ci.yml/badge.svg)](https://github.com/ajac-zero/knack/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/knack.svg)](https://github.com/ajac-zero/knack/blob/main/LICENSE)

`knack` is an open-source Rust CLI and self-hostable registry project for teams to package, validate, version, publish, discover, install, and govern Agent Skills.

The current v0 focuses on local skill authoring and distribution primitives. See [`PROJECT.md`](PROJECT.md) for the north-star product direction.

## Install

Install the prebuilt CLI on macOS or Linux (no Rust toolchain required):

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://raw.githubusercontent.com/ajac-zero/knack/main/scripts/install.sh | sh
```

The installer verifies the release archive SHA-256 checksum and installs to
`~/.local/bin` by default. Set `KNACK_VERSION` to install a specific version or
`KNACK_INSTALL_DIR` to choose another location:

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://raw.githubusercontent.com/ajac-zero/knack/main/scripts/install.sh \
  | KNACK_VERSION=0.3.1 KNACK_INSTALL_DIR=/usr/local/bin sh
```

Prebuilt archives for Linux (x86_64 and ARM64), macOS (Intel and Apple Silicon),
and Windows are available on the [GitHub Releases](https://github.com/ajac-zero/knack/releases)
page. Each archive has a matching `.sha256` checksum file.

If you have Cargo, `cargo-binstall` downloads those same prebuilt archives:

```bash
cargo binstall knack
```

Install the CLI from crates.io as a source-build fallback:

```bash
cargo install knack
```

Install the registry server:

```bash
cargo install knack-registry
```

Or download its prebuilt artifact with `cargo-binstall`:

```bash
cargo binstall knack-registry
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

## MCP server

`knack` includes a stdio [Model Context Protocol](https://modelcontextprotocol.io/)
server so agents can discover and manage skills on a user's behalf. Configure your MCP
client to launch:

```bash
knack mcp
```

For clients that use JSON configuration, the entry typically looks like:

```json
{
  "mcpServers": {
    "knack": {
      "command": "knack",
      "args": ["mcp"]
    }
  }
}
```

The server exposes three tools:

- `find_skills` searches configured HTTP registries and returns ranked matches.
- `add_skill` installs a source and updates the project (or global) manifest and lockfile.
- `publish_skill` validates and publishes a local skill to a configured registry.

All three are enabled by default. Use `--tools` as an explicit allowlist when an MCP
client should have fewer capabilities. For example, a static registry deployment can
expose only discovery and installation:

```bash
knack mcp --tools find_skills,add_skill
```

The valid values are `find_skills`, `add_skill`, and `publish_skill`. This is an MCP
server policy rather than automatic registry detection: knack can search several
registries at once, registry availability can change after startup, and operators may
want to hide a mutating tool even when the backing registry supports it.

The MCP process inherits its working directory and environment from the client. Project
installs therefore apply to that working directory, while `global: true` uses
`~/.agents/`. Publishing reuses credentials from `knack auth login`; trusted automation
can instead provide `KNACK_PUBLISH_TOKEN` in the MCP server environment. Tokens are not
accepted as tool arguments, so they are not exposed to the model.

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

Run the published registry container:

```bash
docker run --rm -p 7349:7349 \
  -v "$PWD:/data:ro" \
  ghcr.io/ajac-zero/knack-registry:<version>
```

Pin a deployment to a specific release tag (or, for maximum immutability, its image
digest). For example, the `knack-registry-v0.3.1` release publishes the `0.3.1` tag.
`latest` tracks the most recent `knack-registry` release. Published images support
`linux/amd64` and `linux/arm64`.

Build from source instead when running unreleased or locally modified code:

```bash
docker build -t knack-registry .
docker run --rm -p 7349:7349 \
  -v "$PWD:/data:ro" \
  knack-registry
```

Both the published and locally built images default to:

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

### Direct publishing (live registry only)

A live registry can also accept skills published directly over HTTP — no backing git repository required. Opt in with a data directory and at least one publish token:

```bash
knack-registry \
  --index knack.index.toml \
  --name company \
  --data-dir /var/lib/knack \
  --publish-token "$TOKEN" \
  --bind 127.0.0.1:7349
```

This enables `PUT /skills/{namespace}/{name}`, authenticated with `Authorization: Bearer <token>`, accepting the exact tarball `knack pack` produces. Uploaded skills are stored under `<data-dir>/skills/<namespace>/<name>/`, folded into the index immediately, and re-indexed from disk on every refresh and restart — put `--data-dir` on a persistent volume. Unlike `--cache-dir` (rebuildable scratch), the data directory holds canonical data.

For a horizontally scaled deployment, use Postgres instead of a local data directory:

```bash
KNACK_DATABASE_URL=postgres://knack:secret@postgres/knack \
knack-registry \
  --name company \
  --publish-token "$TOKEN" \
  --bind 0.0.0.0:7349
```

`KNACK_DATABASE_URL` (or `--database-url`) stores published metadata and archive bytes transactionally in Postgres. All replicas query the shared database for index, search, and archive requests, so a publish accepted by one replica is immediately available through every other replica. The registry creates its `knack_published_skills` table on startup.

For self-service human publishing with attribution, configure OpenID Connect once for the registry instead of issuing per-user registry tokens:

```bash
KNACK_DATABASE_URL=postgres://knack:secret@postgres/knack \
KNACK_OIDC_ISSUER=https://login.example.com/realms/company \
KNACK_OIDC_AUDIENCE=https://skills.example.com \
KNACK_OIDC_CLIENT_ID=knack-cli \
knack-registry --name company --bind 0.0.0.0:7349
```

The provider must issue RS256 JWT access tokens for the configured audience. By default, any authenticated identity able to obtain such a token may publish to its own namespace. If browsing and publishing need separate authorization, set `KNACK_OIDC_REQUIRED_SCOPE=knack:publish`; the registry then requests and validates that scope. Users sign in through the standard authorization-code flow with PKCE:

```bash
knack auth login --registry company
knack publish ./my-skill --registry company
```

The registry derives a personal namespace from the authenticated identity's `preferred_username` claim and claims it transactionally on first publish. No administrator provisioning is needed. The stable ownership key is the token's `(issuer, subject)` pair, so a later username change does not move the namespace. Name collisions receive a deterministic suffix. Published entries expose the authenticated publisher and timestamps in `/index` and `/search`, and every OIDC publish appends an audit event in Postgres.

OIDC publishing deliberately requires Postgres: identity-to-namespace assignments and attribution must be consistent across replicas and restarts. The CLI stores OAuth credentials in `~/.agents/knack-credentials.toml` with mode `0600` on Unix. `knack auth logout --registry company` removes them.

No index file is required for a Postgres-only registry. Pass `--index knack.index.toml` if you also want to merge operator-managed Git sources into the same registry. In that hybrid mode, run each replica with the same index and source aliases; `--cache-dir` remains per-replica, rebuildable Git cache and does not need to be shared. Without Postgres, an omitted `--index` retains the existing default of `knack.index.toml`.

Notes:

- Publishing stays disabled unless a token is configured with either `--data-dir` (single node) or `--database-url` (multiple replicas); without them the server is read-only, the same surface a static snapshot offers. The two storage options are mutually exclusive.
- `--publish-token` is repeatable (one per team, or old+new during rotation); `--publish-tokens-file` reads one token per line for setups where the process list is visible.
- Publish tokens are service credentials for trusted automation. They still require an explicit `--namespace` and are not represented as human attribution. Human self-service publishing requires OIDC and omits `--namespace`.
- Uploads must be namespaced. A publish that would shadow a skill provided by a git-backed `[[skill]]`/`[[source]]` entry is rejected with 409 — the operator-managed index always wins.
- Re-publishing the same `namespace/name` replaces the previous upload (latest-only; no version history yet).
- Uploaded archives have no git provenance, so there's no `X-Knack-Resolved-Sha`; clients fall back to checksum-based change detection in the lockfile.
- `GET /info` advertises `"publish": true|false` so clients can fail fast against read-only registries.
- `--publish-max-bytes` caps the accepted archive size (default 50 MiB).

### Static deployment (Cloudflare R2 + Worker)

For a public, read-mostly registry, the cheapest shape is to skip running a live registry process entirely. Run `knack-registry build-static` from a CI cron job, upload the output to an object store (R2, S3, GCS, anything), and serve it via a small edge worker:

```bash
knack-registry build-static \
    --index registries/public.toml \
    --output ./dist \
    --name public
```

This produces `info.json`, `index.json`, `sha-map.json`, and one `skills/<name>.skill.tar.gz` per indexed skill. The output is everything a knack CLI client needs; an edge function in front of the bucket maps the four CLI endpoints (`/info`, `/index`, `/search`, `/skills/<name>/archive`) onto these files. See [`examples/cloudflare-worker/`](examples/cloudflare-worker/) for a working Worker + R2 setup with a daily GitHub Actions cron, free-tier-friendly at the scale of the public registry (~200 skills, single-digit thousands of requests/day).

Tradeoff: refresh granularity drops from `--refresh-interval-seconds` (default 300s) to whatever your cron interval is (daily for the public registry). Static loses dynamic queries, auth, direct publishing (`knack publish` to an HTTP registry needs the live server's `PUT` endpoint), and the ability to index private sources — keep `knack-registry serve` for internal team registries where any of those matter.

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

Publish a local skill. Two flows, picked implicitly by the kind of registry you pass to `--registry`:

**Git-host registries** — the skill lands as a commit in a skills repository:

```bash
knack registry add git+ssh://git@gitea.example.com tea
knack publish ./my-skill \
  --registry tea \
  --repo platform/agent-skills
```

This clones the target repository, copies the skill into `skills/<skill-name>`, regenerates `knack.index.toml`, commits the change, and pushes it. Use `--no-push` to leave the commit local in the temporary checkout for debugging. After the registry server is serving the updated `knack.index.toml`, teammates can discover and install the skill:

```bash
knack find my-skill
knack add tea:platform/agent-skills/skills/my-skill
```

**HTTP registries** — the skill is uploaded straight to a live `knack-registry` with publishing enabled (see [Direct publishing](#direct-publishing-live-registry-only)), no git repository involved:

```bash
knack auth login --registry company
knack publish ./my-skill \
  --registry company
```

The skill is packed, validated, attributed to the authenticated OIDC identity, and uploaded under the user's automatically assigned personal namespace. The successful response prints the exact `knack add company:<namespace>/my-skill` command. For trusted automation, continue to pass `--namespace platform-team --token "$TOKEN"` (or set `KNACK_PUBLISH_TOKEN`). Static registry snapshots reject publishes with an actionable error.

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
- Direct HTTP publishing to a live registry (`knack publish` →
  `PUT /skills/{ns}/{name}`, token-authenticated, filesystem-backed).
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
