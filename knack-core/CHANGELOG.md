# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1](https://github.com/ajac-zero/knack/compare/knack-core-v0.3.0...knack-core-v0.3.1) - 2026-07-10

### Added

- *(core)* discount common query terms via IDF weighting in search ranking

## [0.3.0](https://github.com/ajac-zero/knack/compare/knack-core-v0.2.2...knack-core-v0.3.0) - 2026-07-10

### Added

- *(cli)* show descriptions, cap and rank find output, tolerate registry failures
- *(core)* [**breaking**] rank find results by match relevance
- *(cli)* default_registry — bare install commands (knack add ns/name) resolve without alias prefix
- *(cli)* namespaced install commands (alias:namespace/name) with legacy fallback
- *(core)* [**breaking**] add namespace field across IndexedSkill + LockedSkill, lockfile v2

## [0.2.2](https://github.com/ajac-zero/knack/compare/knack-core-v0.2.1...knack-core-v0.2.2) - 2026-06-29

### Added

- *(cli)* bootstrap public registry on knack init
- *(registry)* build-static subcommand for offline-baked deployments

### Fixed

- fix(core)/feat(registry): split metadata validation, drop dir-match for indexed skills
- *(core)* drop 1024-character description ceiling

### Other

- daily workflow to publish the public knack registry to R2
- *(registry)* persistent source cache with incremental refresh
- curated source list for the public knack registry

## [0.2.1](https://github.com/ajac-zero/knack/compare/knack-core-v0.2.0...knack-core-v0.2.1) - 2026-06-27

### Fixed

- *(core)* ignore unknown frontmatter fields instead of rejecting them

## [0.2.0](https://github.com/ajac-zero/knack/compare/knack-core-v0.1.0...knack-core-v0.2.0) - 2026-06-27

### Added

- *(core)* add schema version field to lockfile
- *(cli)* [**breaking**] knack registry add resolves name from /info on HTTP registries
- *(cli)* surface registry-add overwrites with a per-field diff
- *(cli)* [**breaking**] infer registry kind from URL scheme
- *(cli)* [**breaking**] move global manifest from ~/.config/knack/ to ~/.agents/
- *(cli)* [**breaking**] knack find and registry list merge all scopes by default
- *(cli)* [**breaking**] replace --scope flag with -g/--global short form
- *(cli)* auto-init manifest on knack add

### Other

- document --check, --dry-run, content-addressed lockfile, --name
- *(cli)* [**breaking**] split sync --update into knack sync and knack update
- rewrite README around installed binaries
