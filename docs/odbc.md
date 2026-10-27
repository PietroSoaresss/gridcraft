# One-shot database imports through ODBC

GridCraft imports PostgreSQL, SQL Server/Azure SQL, MySQL, MariaDB, Oracle and
SQLite 3 through matching Windows ODBC drivers. Desktop, CLI and MCP use the same
engine commands. See the [connector contract](../crates/odbc/README.md) for driver
behavior, read-only protection, limits and adding another database family.

## Desktop

1. Install a driver matching the architecture of GridCraft (usually 64 bit).
2. Create a User or System DSN in **ODBC Data Sources (64-bit)**. DSN names must
   fit 32 UTF-16 code units. Use a database account with SELECT-only permissions.
3. Open **Data → From ODBC**, select/discover a DSN, and enter credentials.
   Alternatively, choose a connection string with a registered driver, such as
   `Driver={PostgreSQL Unicode(x64)};Servername=localhost;Port=5432;Database=example;SSLmode=require;`.
   Use the separate user/password fields. Disable driver/ODBC-manager tracing.
4. Enter one SELECT/WITH/VALUES/TABLE query. **Test Connection** checks access;
   **Preview (100 rows)** leaves the workbook unchanged; **Load as Table** creates
   a table in a new sheet in one undoable edit.

The loaded table is an ordinary snapshot. To fetch current data, rerun the import
with the desired SQL. A script can generate a new output workbook on every run.
Opening a workbook never runs SQL.

## Values and limits

| Database value | Workbook cell |
|---|---|
| NULL | Empty |
| Boolean | Boolean (integer metadata remains numeric 0/1) |
| Integer/decimal within 15 significant digits | Number |
| Larger integers/decimals | Exact text |
| Floating point | Number; nonfinite values remain text |
| Text, GUID/UUID, SQL Server XML | Literal text, including a leading `=` |
| Date | Date serial in the workbook's 1900/1904 calendar, `yyyy-mm-dd` format |
| Time/timestamp | Serial with a time/date format; millisecond display |
| Timestamp with an explicit offset | ISO text retaining the offset |

The connector retains fractional seconds up to nanoseconds; spreadsheet serials
have floating-point precision. Dates outside the workbook calendar and leap
seconds remain text. ODBC timestamps may omit timezones: explicitly cast to text
when preserving the server representation matters. Unsupported/binary types and
nonstandard date/time strings fail; cast them to text in SQL if needed. SQLite
expressions with unknown column metadata stay text. Empty or duplicate headers
become unique table column names; zero rows still produce headers.

Preview is bounded to 100 rows and 2 MiB. Load accepts `maxRows` from 1 to 100,000
(default 100,000), at most 256 columns, 128 MiB including row/cell overhead, and
1 MiB per text cell. Overflow fails without applying a partial table. A changed
workbook rejects stale results. Cancellation prevents application while an
in-flight native call may finish before the worker releases the connection.
Login/statement timeouts are 15/60 seconds where supported by the driver.

## CLI and MCP

| Command | Parameters / result |
|---|---|
| `data.odbcSources` | `{}` → `{dataSources, drivers}` |
| `data.odbcTest` | `{dsn \| connectionString, user?, password?}` → `{jobId}` |
| `data.odbcPreview` | Connection fields plus `sql`, optional `maxRows` → `{jobId}` |
| `data.fromOdbc` | Connection fields plus `sql`, optional `maxRows` → `{jobId}` |
| `data.odbcStatus` | `{jobId}` → state, error/rows/columns/truncated |
| `data.odbcApply` | `{jobId}` → preview values or imported table summary |
| `data.odbcCancel` | `{jobId}` → cancellation request |

MCP `execute_command` and control `engine.execute` can invoke these commands.
Poll until `ready`, then apply. Test ends as `completed`; other terminal states
are `failed` and `cancelled`. Apply/cancel the current ready job before starting
another. These commands never open dialogs programmatically. Import operations
are omitted from replay journals. Passwords/connection strings are masked in
forms and control responses and cleared from completed jobs; there are no saved
connection profiles. Query text, DSNs and credentials are not saved in the workbook.

Both examples require the `Reporting` DSN with suitable authentication for each
invocation. Credentials entered in a desktop session are unavailable to a later
CLI invocation. Avoid credentials in shell history.

```powershell
gridcraft-cli run --cmd 'data.fromOdbc={"dsn":"Reporting","sql":"SELECT id, name FROM public.customers","maxRows":1000}' --out imported.xlsx

# Run again later to produce a current snapshot:
gridcraft-cli run --cmd 'data.fromOdbc={"dsn":"Reporting","sql":"SELECT id, name FROM public.customers","maxRows":1000}' --out current.xlsx
```

The CLI waits for jobs and applies results before writing `--out`. It requests
cancellation and returns an error after five minutes.

## Deferred query model

Saved query definitions, ODBC Refresh/Refresh All, source profiles, fingerprints
and the GridCraft XLSX query extension are preserved on
[`parked/odbc-saved-queries`](https://github.com/PietroSoaresss/gridcraft/tree/parked/odbc-saved-queries).
They are excluded from this import-only delivery while
[#431](https://github.com/storytold/gridcraft/issues/431) discusses a shared
transformation/query model for every source. Existing PivotTable Refresh All
retains its original behavior. Keep copies of experimental workbooks from the
parked branch if their query metadata is needed; this build does not interpret it.
Arrow batches and unixODBC are optional connector follow-ups.

## Contributor validation

Connector tests and dedicated native-driver instructions are in
[`crates/odbc/README.md`](../crates/odbc/README.md). Engine/UI/CLI tests use a
deterministic backend to cover preview, load, cancellation, stale results, undo,
date calendars, literal text and XLSX output without query metadata. Run
`cargo test -p gridcraft-engine --test odbc`,
`cargo test -p gridcraft-ui-egui --test odbc`, and
`cargo test -p gridcraft-cli odbc`, followed by `cargo xtask ci`.

On 2026-10-27, the connector's 17 unit/profile tests, 8 engine tests, 5 UI tests
and CLI import test passed. Revised native PostgreSQL and SQLite tests passed;
a live PostgreSQL → CLI → XLSX check also verified numeric dates with formatting
and no saved query/connection metadata. The combined WASM gate and library
Clippy passed. Full Windows CI remains blocked by upstream's Unix-only I/O test
and the CLI discovery recursion lint. SQL Server, MySQL, MariaDB and Oracle
servers were unavailable for rerunning the revised temporal mappings.
