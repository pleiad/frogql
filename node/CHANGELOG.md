# Changelog

All notable changes to the `frogql` npm package will be documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Released in lock-step with the [PyPI `frogql` package](https://pypi.org/project/frogql/).

## [Unreleased]

## [0.2.0-rc.3] — 2026-05-12

### Fixed

- Host package now ships `index.js` and `index.d.ts`. The `0.2.0-rc.2` tarball uploaded those files as build artifacts only and they never reached the publish job, so the package on npm contained README + LICENSE + package.json with no JavaScript. `require('frogql')` returned `Cannot find module './index.js'` for every consumer.
- Both files are now checked into git (canonical napi-rs pattern). Removed from `node/.gitignore`.

### Deprecated

- `frogql@0.2.0-rc.2` — broken, use `frogql@0.2.0-rc.3` or later.

## [0.2.0-rc.2] — 2026-05-12

### Added

- Initial public release of the napi-rs Node.js bindings for froGQL.
- Module surface: `open(path)`, `importJson(dbPath, jsonPath)`, `importCsv(dbPath, csvDir)`.
- `Connection` class with `nodeCount`, `edgeCount`, `execute(query, limit?)`, `save()`, `schema()`, `graphTypes()`.
- Strong TypeScript types for return shapes: `SchemaSummary`, `GraphTypeSummary`, `NodeRef`, `EdgeRef`, `DmCounters`, `DdlOk`, `IndexResult`, `IndexSummary`.
- Polymorphic `execute()` typed as `unknown` with documented cast targets per statement kind.
- Per-platform prebuilt binaries: macOS x64 + arm64, Linux x64 + arm64 (glibc), Windows x64.
- Pre-release tag mapped to npm dist-tag `next`; stable releases land on `latest`.

### Notes

`-rc.x` indicates the release pipeline (build matrix, multi-package publish, provenance) is being validated. Surface and on-disk format are stable; the next release will be `0.2.0` without the suffix once the platform matrix has shipped one clean tag end-to-end.
