# skillhub

`skillhub` is an open-source Rust CLI and self-hostable registry project for teams to package, validate, version, publish, discover, install, and govern Agent Skills.

The current v0 focuses on local skill authoring and distribution primitives. See [`PROJECT.md`](PROJECT.md) for the north-star product direction.

## Current CLI

Run the CLI from the workspace:

```bash
cargo run -p skillhub-cli -- --help
```

Create a skill:

```bash
cargo run -p skillhub-cli -- new rust-code-review
```

Initialize a project manifest:

```bash
cargo run -p skillhub-cli -- init
```

By default, commands use project scope:

```text
skillhub.toml
skillhub.lock
.agents/skills/
```

Use global scope for user-wide skills:

```bash
cargo run -p skillhub-cli -- init --scope global
cargo run -p skillhub-cli -- add gh:anthropics/skills/skills/pdf --scope global
cargo run -p skillhub-cli -- sync --scope global
cargo run -p skillhub-cli -- list --scope global
```

Global scope uses:

```text
~/.config/skillhub/skillhub.toml
~/.config/skillhub/skillhub.lock
~/.agents/skills/
```

Admins can provide system-wide defaults with system scope:

```bash
cargo run -p skillhub-cli -- init --scope system
cargo run -p skillhub-cli -- registry add tea git+ssh://git@gitea.example.com --scope system
```

System scope uses:

```text
/etc/skillhub/skillhub.toml
/etc/skillhub/skillhub.lock
/usr/local/share/skillhub/skills/
```

Registry aliases are inherited in this order, with later layers overriding earlier layers:

```text
system (/etc/skillhub/skillhub.toml)
global (~/.config/skillhub/skillhub.toml)
project (./skillhub.toml)
```

That lets administrators inject aliases such as `tea:` for all users while still allowing user or project-level overrides.

Add and install a skill source into the manifest target:

```bash
cargo run -p skillhub-cli -- add gh:anthropics/skills/skills/pdf
```

This updates `skillhub.toml` and writes `skillhub.lock` with the resolved source and a deterministic checksum of the installed skill contents.

Sync all skills declared in `skillhub.toml`:

```bash
cargo run -p skillhub-cli -- sync
```

Validate a skill:

```bash
cargo run -p skillhub-cli -- validate rust-code-review
```

Package a skill:

```bash
cargo run -p skillhub-cli -- pack rust-code-review --output dist
```

Install a skill directory or package archive:

```bash
cargo run -p skillhub-cli -- install rust-code-review --target .agents/skills
cargo run -p skillhub-cli -- install dist/rust-code-review.skill.tar.gz --target .agents/skills
```

Install directly from a public GitHub repository:

```bash
cargo run -p skillhub-cli -- install gh:owner/repo/path/to/skill --target .agents/skills
cargo run -p skillhub-cli -- install gh:owner/repo@ref/path/to/skill --target .agents/skills
```

For example:

```bash
cargo run -p skillhub-cli -- install gh:anthropics/skills/skills/pdf --target .agents/skills
```

Install from any Git host supported by your local `git` command:

```bash
cargo run -p skillhub-cli -- install git+https://git.example.com/org/skills.git//path/to/skill
cargo run -p skillhub-cli -- install git+ssh://git@git.example.com/org/skills.git@main//path/to/skill
```

Register a Git host alias in `skillhub.toml`:

```bash
cargo run -p skillhub-cli -- registry add tea git+ssh://git@gitea.example.com
cargo run -p skillhub-cli -- registry list
```

Then add skills through the alias:

```bash
cargo run -p skillhub-cli -- add tea:platform/agent-skills/rust-code-review
cargo run -p skillhub-cli -- add tea:platform/agent-skills@v1.2.0/rust-code-review
```

Alias syntax is:

```text
alias:owner/repo[@ref]/path/to/skill
```

Generate a searchable registry index from a local tree of skills:

```bash
cargo run -p skillhub-cli -- index generate ./skills \
  --source-prefix gh:owner/repo/skills \
  --output skillhub.index.toml
```

Serve the index with `skillhub-registry`:

```bash
cargo run -p skillhub-registry -- --index skillhub.index.toml --bind 127.0.0.1:7349
```

Register and search that registry from the CLI:

```bash
cargo run -p skillhub-cli -- registry add local http://127.0.0.1:7349 --kind http
cargo run -p skillhub-cli -- find pdf
```

Search results are installable sources:

```text
local	pdf	Work with PDF files...	gh:owner/repo/skills/pdf
```

List installed skills:

```bash
cargo run -p skillhub-cli -- list --target .agents/skills
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
- Searchable HTTP registries served by `skillhub-registry`.
- Registry index generation from local skill directories.
- Project manifests with `skillhub.toml`.
- Lockfiles with `skillhub.lock`.
- Project and global scoped config/install paths.
- System scoped config at `/etc/skillhub/skillhub.toml`.
- Layered registry alias inheritance from system to global to project.
- `add` and `sync` workflows for reproducible project installs.
- Listing installed skills.

Not implemented yet:

- Static registries.
- Locking GitHub branches and tags to immutable commit SHAs.
- Registry server behavior.
- Publishing.
- Signing, provenance, and policy checks.
