# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
