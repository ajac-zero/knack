# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/ajac-zero/knack/compare/knack-v0.2.2...knack-v0.3.0) - 2026-07-01

### Added

- *(cli)* default_registry — bare install commands (knack add ns/name) resolve without alias prefix
- *(cli)* surface namespace in 'knack find' output; install command uses qualified name
- *(cli)* namespaced install commands (alias:namespace/name) with legacy fallback
- *(core)* [**breaking**] add namespace field across IndexedSkill + LockedSkill, lockfile v2

## [0.2.2](https://github.com/ajac-zero/knack/compare/knack-v0.2.1...knack-v0.2.2) - 2026-06-29

### Added

- *(cli)* bootstrap public registry on knack init
- *(registry)* build-static subcommand for offline-baked deployments
- *(cli)* sync falls back to manifest source when pinned SHA is gone
- *(cli)* knack update [<skill>...] for targeted updates

### Other

- daily workflow to publish the public knack registry to R2
- *(registry)* persistent source cache with incremental refresh
- curated source list for the public knack registry

## [0.2.1](https://github.com/ajac-zero/knack/compare/knack-v0.2.0...knack-v0.2.1) - 2026-06-27

### Fixed

- *(cli)* consult project manifest for alias resolution under -g

### Other

- colourize --help output in both binaries
- *(cli)* align unknown-registry hints with new `<url> [<name>]` order

## [0.2.0](https://github.com/ajac-zero/knack/compare/knack-v0.1.0...knack-v0.2.0) - 2026-06-27

### Added

- *(core)* add schema version field to lockfile
- *(cli)* knack sync --check verifies lockfile + install integrity
- *(cli)* knack update --dry-run reports what would change
- *(cli)* record HTTP registry SHA in lockfile via X-Knack-Resolved-Sha
- *(cli)* pin lockfile resolved field to commit SHA when available
- *(cli)* knack sync --update honors pinned refs, --force overrides
- *(cli)* [**breaking**] knack registry add resolves name from /info on HTTP registries
- *(cli)* knack sync --update re-resolves and reinstalls
- *(cli)* suggest concrete fixes on gh: source 404
- *(cli)* suggest knack find on 404 from HTTP registry add
- *(cli)* suggest known registries when alias is unknown
- *(cli)* surface registry-add overwrites with a per-field diff
- *(cli)* [**breaking**] infer registry kind from URL scheme
- *(cli)* [**breaking**] knack list shows project and global skills by default
- *(cli)* auto-init manifest on knack registry add
- *(cli)* [**breaking**] move global manifest from ~/.config/knack/ to ~/.agents/
- *(cli)* [**breaking**] knack find and registry list merge all scopes by default
- *(cli)* [**breaking**] replace --scope flag with -g/--global short form
- *(cli)* auto-init manifest on knack add
- *(cli)* style output with anstream

### Fixed

- *(cli)* clone SHA refs via full clone + checkout in fetch_git_skill
- *(cli)* silence git clone in fetch_git_skill via run_git
- *(cli)* silence git output on success, surface stderr on failure
- *(cli)* preserve publish --no-push working tree for inspection
- resolve preexisting clippy lints

### Other

- document --check, --dry-run, content-addressed lockfile, --name
- *(cli)* [**breaking**] split sync --update into knack sync and knack update
- *(cli)* move 'generated index' status to the index command
- *(cli)* use kebab-case kind labels in registry list
- rewrite README around installed binaries
