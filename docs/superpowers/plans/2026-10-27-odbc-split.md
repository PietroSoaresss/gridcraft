# ODBC review split

The complete implementation is preserved at `parked/odbc-saved-queries`
(`2b3d5c890f3d076a4f4b0cfba6cd6071ea4046bb`). This plan implements the PR
feedback without choosing the saved-query contract being discussed in #431.

## 1. Standalone connector

- Branch `feat/odbc-connector`, based on upstream main.
- Keep `crates/odbc`, workspace registration and layer registration only, with
  contributor documentation. No engine, UI or workbook changes.
- Contract: `Backend::query(connection, sql, limits, preview, cancelled)` returns
  a bounded typed `DataSet`. Replace spreadsheet cell values with connector values;
  return dates, times and timestamps as Chrono types. Preserve precision, nulls,
  Unicode, credentials redaction, vendor guards and rollback.
- Cover typed temporal results and existing six-driver behavior. Run connector
  tests and repository CI before the English commit.

## 2. One-shot import

- Keep PR #1717 on `feat/odbc-postgresql`; merge the connector branch normally.
- Remove QuerySource, query profiles, refresh jobs, fingerprint/replace logic,
  XLSX extensions and their tests. Restore existing pivot refresh behavior.
- Retain Test, Preview, Load, async jobs, cancellation, undo, CLI and MCP commands.
- Convert connector dates to the workbook date system and apply number formats.
  Preserve offsets as text and dates outside spreadsheet limits as text.
- Update command docs, user docs and tests. Prove saved files contain ordinary
  tables without SQL, connection settings or private query metadata.
- Run affected tests, format/layer/parity checks and repository CI, then commit
  in English and push without rewriting published history.

## 3. Review delivery

- Open a connector-only upstream PR and describe the dependency in PR #1717.
- Link the preserved branch and #431 in the PR descriptions.
- Request a fresh local review of the combined change; fix material findings and
  report actual validation, including unavailable environments.

## Deferred

Arrow record batches and unixODBC are optional follow-ups. Do not introduce a
Polars pipeline, saved transformations, refresh contract or workbook extension.

## Execution notes

- Connector: 17 unit/profile tests passed; revised PostgreSQL and SQLite native
  integration tests passed on 2026-10-27. Other four local servers are absent.
- Connector CI: format and connector Clippy passed. Full CI is blocked by unchanged
  upstream Windows issues: `engine/src/io/tests.rs:32` uses `std::os::unix`, and
  `apps/gridcraft-cli/src/check.rs:281` triggers `only_used_in_recursion` with
  Rust 1.99. Assets and layers passed when run separately.
- Fresh review found historical years must be checked before Excel DATE conversion;
  importer now preserves pre-epoch dates/timestamps as text, with regression cases.
