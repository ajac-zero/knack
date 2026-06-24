# skillhub

`skillhub` is an open-source Rust CLI and self-hostable registry for teams to package, validate, version, publish, discover, install, and govern Agent Skills.

Agent Skills are portable folders built around a `SKILL.md` file that give AI agents reusable capabilities, workflows, and domain knowledge. `skillhub` turns those folders into manageable team infrastructure: searchable, reproducible, auditable, and safe to distribute across projects and agent clients.

## North Star

Make Agent Skills as easy and trustworthy to share inside a team as packages are in a language ecosystem.

The project should feel like a focused package manager for agent capabilities: simple for individuals, reliable for teams, and secure enough for organizations that treat agent instructions as part of their software supply chain.

## Problem

Teams are starting to encode important operational knowledge as agent skills: code review standards, release procedures, incident runbooks, deployment workflows, compliance checks, internal tool instructions, and project-specific conventions.

Without a dedicated tool, these skills tend to become scattered across personal directories, project folders, vendor-specific client locations, prompt libraries, wikis, and internal Git repositories. That makes them hard to discover, update, review, version, and trust.

The missing layer is shared distribution and governance.

## Purpose

`skillhub` exists to help teams:

- Create valid Agent Skills quickly.
- Validate skills against the open Agent Skills format and team policy.
- Package skills into deterministic, versioned artifacts.
- Publish skills to a private, self-hosted registry.
- Discover useful skills through search and metadata.
- Install skills into project-level or user-level skill directories.
- Pin skill versions for reproducible agent behavior.
- Share curated skill bundles for teams, repos, and onboarding.
- Audit, verify, deprecate, and remove unsafe or outdated skills.

## Value

`skillhub` should deliver value in four areas.

### Reuse

Teams should be able to write a skill once and install it across many repositories, users, and compatible agent clients.

### Reproducibility

Projects should be able to declare and lock the exact skills they expect, so agent behavior is not dependent on whatever happens to be installed on a developer's machine.

### Governance

Organizations should know which skills exist, who owns them, which versions are approved, what changed between releases, and where they are installed.

### Trust

Agent instructions can influence tool use and code changes. `skillhub` should treat skill distribution as a supply-chain problem, with checksums, provenance, policy checks, and eventually signing and approval workflows.

## Differentiators

`skillhub` is not just a folder copier, prompt library, or vendor-specific marketplace.

The differentiators are:

- Vendor-neutral support for the open Agent Skills format.
- Self-hosted by default, suitable for private team and company knowledge.
- Rust-native CLI and registry, distributed as fast single binaries.
- Reproducible installs through manifests, lockfiles, and checksums.
- Cross-client installation targets, starting with `.agents/skills/`.
- Team bundles for onboarding and role-specific skill sets.
- Built-in validation, linting, and policy checks.
- Security-focused design for scripts, provenance, and trusted registries.
- A static or Git-backed registry path before requiring a full server.

The short pitch: `skillhub` is private package management for Agent Skills.

## Users

### Individual Developers

Developers want to search, install, update, and remove useful skills without knowing where each agent client stores them.

Important workflows:

- `skillhub search rust`
- `skillhub install platform/rust-code-review`
- `skillhub list`
- `skillhub update`

### Engineering Teams

Teams want shared skills for repo conventions, code review, release steps, incident response, and local development workflows.

Important workflows:

- Declare project skills in a manifest.
- Commit a lockfile.
- Install the same skills in CI and local development.
- Publish team-owned skills to an internal registry.
- Create bundles for onboarding.

### AI Platform and Developer Experience Teams

Platform teams want to standardize skills across an organization and keep them discoverable, reviewed, and compatible with multiple agent clients.

Important workflows:

- Operate a private registry.
- Define publishing policy.
- Curate approved bundles.
- Audit installed and published skills.
- Provide bootstrap commands for new teams.

### Security and Compliance Teams

Security teams want visibility and control over agent instructions and bundled scripts.

Important workflows:

- Require validation and approval before publishing.
- Scan skills for secrets or risky scripts.
- Verify checksums and signatures.
- Deprecate or yank unsafe versions.
- Audit provenance and ownership.

## Product Shape

`skillhub` has two main components.

### CLI

The CLI is the primary user experience and should be useful even before a full registry server exists.

Initial command areas:

- `new`: scaffold a skill.
- `validate`: check Agent Skills format compliance.
- `lint`: check project conventions and common quality issues.
- `pack`: create a deterministic skill artifact.
- `install`: install from local path, artifact, Git/static registry, or server registry.
- `sync`: install the versions declared by a project manifest and lockfile.
- `list`: show installed skills.
- `search`: search configured registries.
- `publish`: publish to a configured registry.
- `registry`: manage registry configuration.
- `doctor`: diagnose local client support and install paths.

The CLI should default to the cross-client `.agents/skills/` convention and later support client-specific targets when useful.

### Registry

The registry stores skill metadata and artifacts.

The project should support a low-friction registry path first:

- Static index hosted by Git, S3, GitHub Pages, or any HTTP server.
- Deterministic packaged artifacts with checksums.
- Searchable metadata index.

A full self-hosted server can follow:

- Rust HTTP service.
- SQLite for simple deployments.
- Postgres for team deployments.
- Filesystem and S3-compatible artifact storage.
- Auth tokens initially, OIDC later.
- Namespace and owner management.
- Audit logs and policy enforcement.

## Core Concepts

### Skill

A skill is a valid Agent Skills directory containing `SKILL.md` and optional `scripts/`, `references/`, `assets/`, and other supporting files.

Installed skills should remain portable. A user should be able to copy an installed skill folder into `.agents/skills/` and have it work in any compatible client.

### Package

A package is a versioned skill artifact suitable for distribution through `skillhub`.

The package may include optional registry metadata, but `SKILL.md` remains the canonical runtime entrypoint.

### Registry

A registry is a source of package metadata and artifacts. Registries can be static, Git-backed, or served by the `skillhub` registry server.

### Manifest

A project manifest declares which skills a project wants.

Potential file: `skills.toml`

```toml
[registry]
default = "internal"

[install]
scope = "project"
target = ".agents/skills"

[skills]
"platform/rust-code-review" = "1.4.2"
"sre/incident-triage" = "0.9.3"
```

### Lockfile

A lockfile records exact resolved versions, sources, and checksums.

Potential file: `skills.lock`

```toml
[[package]]
namespace = "platform"
name = "rust-code-review"
version = "1.4.2"
source = "registry+https://skills.example.com"
checksum = "sha256:..."
```

### Bundle

A bundle is a curated set of skills for a team, role, repository type, or onboarding path.

Examples:

- `platform/backend-engineering`
- `sre/on-call`
- `security/review-baseline`
- `frontend/react-product`

## MVP

The first useful version should avoid heavy infrastructure and prove the core lifecycle.

MVP goals:

- Scaffold a new skill.
- Validate `SKILL.md` frontmatter and directory rules.
- Package a skill into a deterministic archive.
- Install a skill from a local path or artifact.
- Install into `.agents/skills/` by default.
- Maintain a project manifest and lockfile.
- Sync a project from its manifest and lockfile.
- Support a static registry index with metadata and artifact URLs.
- Search and install from configured static registries.
- Verify package checksums on install.

MVP non-goals:

- Full web UI.
- Complex authentication.
- Multi-tenant registry server.
- Signing and provenance beyond checksums.
- Automated semantic evaluation of skill quality.
- Supporting every agent client-specific install path.

## Later Roadmap

Important later capabilities:

- Self-hosted registry server.
- Auth tokens, namespaces, owners, and publish permissions.
- OIDC support.
- Skill signing and verification.
- Provenance from GitHub Actions, GitLab CI, or other CI systems.
- Policy engine for organization rules.
- Secret scanning and script risk scanning.
- Skill diffing between versions.
- Deprecation and yank flows.
- Bundle publishing and installation.
- Air-gapped export and import.
- Web UI for browsing, reviewing, and approving skills.
- Client-specific install adapters.
- Evaluation fixtures for activation and behavior testing.

## Design Principles

- Keep the Agent Skills format portable and central.
- Prefer simple workflows before enterprise workflows.
- Make the CLI valuable without requiring a server.
- Make installs reproducible and auditable by default.
- Treat skill scripts and instructions as security-sensitive.
- Avoid vendor lock-in.
- Favor deterministic artifacts and explicit metadata.
- Use clear errors and actionable diagnostics.
- Build small, composable Rust crates as the project grows.

## Open Questions

- Should the first registry backend be static HTTP, Git, or both?
- What package archive extension should be used: `.skill.tgz`, `.skill.tar.zst`, or another format?
- Should registry metadata live only in `SKILL.md` frontmatter at first, or should `skillhub` introduce an optional `skill.toml` package manifest?
- Should project manifests be named `skills.toml`, `skillhub.toml`, or something else?
- How strict should validation be by default versus compatibility mode?
- Which client-specific install targets should be supported after `.agents/skills/`?

## Success Criteria

`skillhub` is succeeding if:

- A team can create, publish, and install a skill in minutes.
- A repository can declare its expected skills and reproduce them on another machine.
- Users can discover internal skills without searching wikis or Slack.
- Platform teams can curate and distribute approved skill bundles.
- Security teams can inspect, verify, and eventually enforce policy on skills.
- Installed skills remain plain Agent Skills that work outside `skillhub`.
