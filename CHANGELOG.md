# Changelog

Notable changes to ccmon. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `ccmon-core`: transcript, runtime-file, and task-list ingest; the session
  state machine; git collection and commit attribution; the work report.
- `ccmon` CLI: `report`, `ls`, `reindex`, `doctor`, `backup`.
- Desktop app: tray icon with a live needs-action badge and native menu, plus a
  window with Sessions, Report, and Settings.
- `--until` on `ccmon report`, so a window can be closed at both ends for
  writing up a period already passed.
- Credential redaction over report output, on by default (`redact_secrets`).

### Security

- Session ids are validated against a strict alphabet before they can reach a
  command line, and both the id and the working directory are quoted for the
  shell that will parse them. Previously the id was interpolated unquoted,
  which allowed command injection from a crafted `sessionId` on disk.

[Unreleased]: https://github.com/shamis6ali/ccmon/compare/main...HEAD
