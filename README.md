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
- Project manifests with `skillhub.toml`.
- Lockfiles with `skillhub.lock`.
- `add` and `sync` workflows for reproducible project installs.
- Listing installed skills.

Not implemented yet:

- Static registries.
- Locking GitHub branches and tags to immutable commit SHAs.
- Registry server behavior.
- Publishing.
- Signing, provenance, and policy checks.
