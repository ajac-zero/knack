# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Add a Postgres-backed direct-publish mode for horizontally scaled live registry deployments.

## [0.3.1](https://github.com/ajac-zero/knack/compare/knack-registry-v0.3.0...knack-registry-v0.3.1) - 2026-07-10

### Added

- *(core)* discount common query terms via IDF weighting in search ranking

## [0.3.0](https://github.com/ajac-zero/knack/compare/knack-registry-v0.2.2...knack-registry-v0.3.0) - 2026-07-10

### Added

- *(cli)* show descriptions, cap and rank find output, tolerate registry failures
- *(registry)* surface match score in search results
- *(cli)* default_registry — bare install commands (knack add ns/name) resolve without alias prefix
- *(registry)* [**breaking**] end-to-end namespacing — materialize, routes, and build-static layout
- *(core)* [**breaking**] add namespace field across IndexedSkill + LockedSkill, lockfile v2

## [0.2.2](https://github.com/ajac-zero/knack/compare/knack-registry-v0.2.1...knack-registry-v0.2.2) - 2026-06-29

### Added

- *(cli)* bootstrap public registry on knack init
- *(registry)* build-static subcommand for offline-baked deployments
- *(registry)* skip invalid skills during materialize, keep going
- *(registry)* resolve gh: sources directly against github.com

### Fixed

- fix(core)/feat(registry): split metadata validation, drop dir-match for indexed skills

### Other

- daily workflow to publish the public knack registry to R2
- *(registry)* persistent source cache with incremental refresh
- *(registry)* partial+sparse clone for sources with a subpath
- curated source list for the public knack registry

## [0.2.1](https://github.com/ajac-zero/knack/compare/knack-registry-v0.2.0...knack-registry-v0.2.1) - 2026-06-27

### Other

- colourize --help output in both binaries

## [0.2.0](https://github.com/ajac-zero/knack/compare/knack-registry-v0.1.0...knack-registry-v0.2.0) - 2026-06-27

### Added

- *(registry)* expose backing-source commit SHA via X-Knack-Resolved-Sha
- *(cli)* [**breaking**] knack registry add resolves name from /info on HTTP registries
- *(registry)* [**breaking**] rename --public-alias to --name and add /info endpoint
- *(cli)* [**breaking**] infer registry kind from URL scheme
- *(cli)* [**breaking**] move global manifest from ~/.config/knack/ to ~/.agents/
- *(cli)* [**breaking**] knack find and registry list merge all scopes by default
- *(cli)* [**breaking**] replace --scope flag with -g/--global short form
- *(cli)* auto-init manifest on knack add

### Fixed

- *(registry)* silence git clone output in fetch_source_root
- resolve preexisting clippy lints

### Other

- document --check, --dry-run, content-addressed lockfile, --name
- *(cli)* [**breaking**] split sync --update into knack sync and knack update
- rewrite README around installed binaries
