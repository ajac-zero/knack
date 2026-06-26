# knack

`knack` is an open-source Rust CLI and self-hostable registry project for teams to package, validate, version, publish, discover, install, and govern Agent Skills.

The current v0 focuses on local skill authoring and distribution primitives. See [`PROJECT.md`](PROJECT.md) for the north-star product direction.

## Current CLI

Run the CLI from the workspace:

```bash
cargo run -p knack-cli -- --help
```

Create a skill:

```bash
cargo run -p knack-cli -- new rust-code-review
```

Initialize a project manifest:

```bash
cargo run -p knack-cli -- init
```

By default, commands use project scope:

```text
.agents/knack.toml
.agents/knack.lock
.agents/skills/
```

Use global scope for user-wide skills:

```bash
cargo run -p knack-cli -- init --scope global
cargo run -p knack-cli -- add gh:anthropics/skills/skills/pdf --scope global
cargo run -p knack-cli -- sync --scope global
cargo run -p knack-cli -- list --scope global
```

Global scope uses:

```text
~/.config/knack/knack.toml
~/.config/knack/knack.lock
~/.agents/skills/
```

Admins can provide system-wide defaults with system scope:

```bash
cargo run -p knack-cli -- init --scope system
cargo run -p knack-cli -- registry add tea git+ssh://git@gitea.example.com --scope system
```

System scope uses:

```text
/etc/knack/knack.toml
/etc/knack/knack.lock
/usr/local/share/knack/skills/
```

Registry aliases are inherited in this order, with later layers overriding earlier layers:

```text
system (/etc/knack/knack.toml)
global (~/.config/knack/knack.toml)
project (./.agents/knack.toml)
```

That lets administrators inject aliases such as `tea:` for all users while still allowing user or project-level overrides.

Add and install a skill source into the manifest target:

```bash
cargo run -p knack-cli -- add gh:anthropics/skills/skills/pdf
```

This updates `.agents/knack.toml` and writes `.agents/knack.lock` with the resolved source and a deterministic checksum of the installed skill contents.

Sync all skills declared in `.agents/knack.toml`:

```bash
cargo run -p knack-cli -- sync
```

Validate a skill:

```bash
cargo run -p knack-cli -- validate rust-code-review
```

Package a skill:

```bash
cargo run -p knack-cli -- pack rust-code-review --output dist
```

Install a skill directory or package archive:

```bash
cargo run -p knack-cli -- install rust-code-review --target .agents/skills
cargo run -p knack-cli -- install dist/rust-code-review.skill.tar.gz --target .agents/skills
```

Install directly from a public GitHub repository:

```bash
cargo run -p knack-cli -- install gh:owner/repo/path/to/skill --target .agents/skills
cargo run -p knack-cli -- install gh:owner/repo@ref/path/to/skill --target .agents/skills
```

For example:

```bash
cargo run -p knack-cli -- install gh:anthropics/skills/skills/pdf --target .agents/skills
```

Install from any Git host supported by your local `git` command:

```bash
cargo run -p knack-cli -- install git+https://git.example.com/org/skills.git//path/to/skill
cargo run -p knack-cli -- install git+ssh://git@git.example.com/org/skills.git@main//path/to/skill
```

Register a Git host alias in `knack.toml`:

```bash
cargo run -p knack-cli -- registry add tea git+ssh://git@gitea.example.com
cargo run -p knack-cli -- registry list
```

Then add skills through the alias:

```bash
cargo run -p knack-cli -- add tea:platform/agent-skills/rust-code-review
cargo run -p knack-cli -- add tea:platform/agent-skills@v1.2.0/rust-code-review
```

Alias syntax is:

```text
alias:owner/repo[@ref]/path/to/skill
```

Generate a searchable registry index from a local tree of skills:

```bash
cargo run -p knack-cli -- index generate ./skills \
  --source-prefix gh:owner/repo/skills \
  --output knack.index.toml
```

Serve the index with `knack-registry`:

```bash
cargo run -p knack-registry -- --index knack.index.toml --bind 127.0.0.1:7349
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
  --public-alias company \
  --bind 0.0.0.0:7349
```

Override arguments as needed:

```bash
docker run --rm -p 7349:7349 \
  -v "$PWD:/data:ro" \
  knack-registry \
  --index /data/knack.index.toml \
  --skills-root /data/skills \
  --public-alias platform \
  --bind 0.0.0.0:7349
```

To make the HTTP registry the only thing users need to interact with, serve skill archives too:

```bash
cargo run -p knack-registry -- \
  --index knack.index.toml \
  --skills-root ./skills \
  --public-alias company \
  --bind 127.0.0.1:7349
```

With `--public-alias company`, search results return proxy install sources such as `company:deploy-container`. The CLI resolves those through the HTTP registry and downloads the skill archive from the registry server.

Register and search that registry from the CLI:

```bash
cargo run -p knack-cli -- registry add local http://127.0.0.1:7349 --kind http
cargo run -p knack-cli -- find pdf
```

Search results are installable sources:

```text
local	pdf	Work with PDF files...	gh:owner/repo/skills/pdf
```

In proxy mode, results look like this:

```text
company	deploy-container	Deploy containers to Kubernetes. 	company:deploy-container
```

Users can install without knowing the backing Git repo:

```bash
cargo run -p knack-cli -- add company:deploy-container
```

For skills scattered across one or more Git repositories, prefer dynamic source entries. The registry fetches the backing source, scans for `SKILL.md`, and derives each skill's name and description from the skill itself:

```toml
[[source]]
source = "tea:platform/agent-skills"
tags = ["deploy", "kubernetes"]
```

At startup, that materializes skills such as `tea:platform/agent-skills/deploy-container` from `platform/agent-skills/deploy-container/SKILL.md`. This avoids duplicating fragile metadata in `knack.index.toml`.

Dynamic sources must refresh successfully on startup before the registry serves traffic. After startup, the registry refreshes dynamic sources every 300 seconds by default and keeps serving the last good index if a later refresh fails. Disable background refresh with `--refresh-interval-seconds 0` or tune it with another interval.

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
cargo run -p knack-registry -- \
  --index knack.index.toml \
  --public-alias company \
  --source-alias tea=git+ssh://git@gitea.example.com \
  --refresh-interval-seconds 300 \
  --bind 127.0.0.1:7349
```

Users still only need the HTTP registry:

```bash
cargo run -p knack-cli -- registry add company http://127.0.0.1:7349 --kind http
cargo run -p knack-cli -- find deploy
cargo run -p knack-cli -- add company:deploy-container
```

Publish a local skill into a git-backed team skills repository:

```bash
cargo run -p knack-cli -- registry add tea git+ssh://git@gitea.example.com
cargo run -p knack-cli -- publish ./my-skill \
  --registry tea \
  --repo platform/agent-skills
```

Publishing currently supports `git-host` registries. It clones the target repository, copies the skill into `skills/<skill-name>`, regenerates `knack.index.toml`, commits the change, and pushes it. Use `--no-push` to leave the commit local in the temporary checkout for debugging.

After the registry server is serving the updated `knack.index.toml`, teammates can discover and install the skill:

```bash
cargo run -p knack-cli -- find my-skill
cargo run -p knack-cli -- add tea:platform/agent-skills/skills/my-skill
```

List installed skills:

```bash
cargo run -p knack-cli -- list --target .agents/skills
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
- `add` and `sync` workflows for reproducible project installs.
- Listing installed skills.

Not implemented yet:

- Static registries.
- Locking GitHub branches and tags to immutable commit SHAs.
- Signing, provenance, and policy checks.
