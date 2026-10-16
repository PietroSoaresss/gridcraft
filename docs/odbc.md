# PostgreSQL through ODBC

GridCraft can preview, load and manually refresh PostgreSQL query results on Windows.
Desktop, CLI and MCP use the same engine commands. Other platforms can edit the saved
cells and preserve query definitions; they cannot connect to a local ODBC driver yet.

## Setup and desktop workflow

1. Install [psqlODBC](https://odbc.postgresql.org/) for the architecture of GridCraft
   (usually 64 bit). The PostgreSQL server installer does not necessarily install this driver.
2. In Windows **ODBC Data Sources (64-bit)**, create a PostgreSQL Unicode User or System DSN
   with your server, database and SSL settings. Use a DSN name of at most 32 characters.
3. Open **Data → From ODBC**, discover/select the DSN and enter your username and password.
   Alternatively, choose connection string and enter a string such as
   `Driver={PostgreSQL Unicode(x64)};Server=localhost;Port=5432;Database=example;SSLmode=require;`.
   The driver name must match a registered driver. Use separate credential fields.
4. Enter a query, then **Test**, **Preview**, or **Load**. Loading creates a table in a new sheet.
5. Use **Data → Connections** to preview or refresh a query table. **Refresh All** updates query
   tables before refreshing PivotTables. A failed query keeps its previous cells.

Use a database account with only the required SELECT privileges. GridCraft executes queries
in a PostgreSQL read-only transaction and rolls it back. It also forces driver read-only mode,
disables driver communication/debug logs and clears connection initialization SQL. Driver options
and SSL behavior are documented by [psqlODBC](https://odbc.postgresql.org/docs/config.html).
Cursor fetching is forced with a 100-row driver cache; accepted SQL is wrapped as a SELECT so
TABLE/VALUES also use that path. The byte limits below apply to GridCraft's materialized result;
the native driver may allocate additional memory for its current page and large fields.

## Queries, values and limits

Queries must be one `SELECT`, `WITH`, `VALUES` or `TABLE` statement. One trailing semicolon is
allowed; internal semicolons (including in literals/comments) are currently rejected.
Binary and unsupported ODBC column types return an error. Cast them explicitly to text if needed.

| Value | Imported cell |
|---|---|
| SQL NULL | Empty |
| Boolean | Boolean |
| Integer/decimal within 15 significant digits | Number |
| Larger integers/decimals | Exact text, preserving precision |
| Floating point | Number; nonfinite values remain text |
| Text, dates and timestamps | Text supplied by the driver |

Text beginning with `=` stays literal text. Date/time strings are not converted to spreadsheet
serial dates. ODBC timestamp types may omit timezone information; select an explicit textual
representation when preserving timezone/offset matters. Duplicate or empty column names are
normalized into unique table headers. A query with zero rows still creates headers.

Preview is capped at 100 rows and 2 MiB. Loads allow 1–100,000 rows (default 100,000),
256 columns, 128 MiB of materialized storage including cell/row overhead, and 1 MiB per cell.
Preview may return `truncated: true`. Loads and refreshes exceeding a row or size limit fail
without applying partial data; restrict the SQL or increase `maxRows`. Login timeout is 15 seconds and statement timeout is 60 seconds,
subject to driver support. Cancellation prevents application immediately, but an in-flight
native call may finish before the worker releases the connection.

## Refresh and persistence

Load and refresh apply through one undoable engine edit. A result cannot apply after the workbook
changes or another document becomes active. Refresh keeps styles and filter criteria, resizes
the table and clears only its previous data area. It rejects changed column names/types,
formulas, merges, spilled arrays, pictures, protected sheets and occupied expansion areas.
If you edited cached values, refresh requires the explicit **Replace edited data** option.
This option does not override formula or adjacent-data protections.
Refresh also refuses to reapply a query filter when another active filter shares its rows.
Reload into a new table to change the stored SQL or row cap.

Query definitions contain the SQL, DSN (when used), row cap, column types and cached-data
fingerprint. Serialized model JSON and a GridCraft XLSX table extension preserve these definitions; Excel does
not refresh this extension. Opening a workbook never executes SQL. SQL and DSN names are
saved in the workbook, so do not embed secrets in SQL.

Passwords and arbitrary connection strings are kept only in memory, are masked in dialogs,
and are omitted from journals, dialog inspection and workbook files. After reopening, reenter
credentials; for a connection-string source, reenter that string too. Up to 64 live source
profiles and 32 terminal jobs are retained per session. Closing a document discards its profiles.

## Engine / MCP

Use MCP `execute_command` or control-channel `engine.execute` with these command IDs:

| Command | Parameters / result |
|---|---|
| `data.odbcSources` | `{}` → `{dataSources, drivers}` |
| `data.odbcTest` | `{dsn \| connectionString, user?, password?}` → `{jobId}` |
| `data.odbcPreview` | Connection fields plus `sql`, optional `maxRows` → `{jobId}` |
| `data.fromOdbc` | Connection fields plus `sql`, optional `maxRows` → `{jobId}` |
| `data.odbcStatus` | `{jobId}` → state, error/row count/column count/truncation |
| `data.odbcApply` | `{jobId, replace?}` → preview data or applied table summary |
| `data.odbcCancel` | `{jobId}` → cancellation request |
| `data.refreshQuery` | `{table, dsn? \| connectionString?, user?, password?, replace?}` → `{jobId}` |
| `data.refreshAll` | Optional connection overrides and `replace` → `{jobId}` when queries exist |

Poll jobs until `ready`, then call `data.odbcApply`. Connection tests finish as `completed`.
Other terminal states are `failed` and `cancelled`. Only one running or ready job is allowed;
apply or cancel a ready result before submitting another. Preview/test also accept `table` to
reuse a query definition and its live profile. Query jobs are not recorded as replay scripts.

Refresh All fetches queries sequentially within a shared 128 MiB budget, applies successful
queries in one edit, then refreshes pivots. The result includes `queriesRefreshed`,
`pivotsRefreshed`, `errors`, `truncated` and `truncatedQueries`. Failed queries keep their cache.
With no query tables, the existing synchronous PivotTable response is unchanged.

## CLI

```powershell
gridcraft-cli run --cmd 'data.fromOdbc={"dsn":"Reporting","sql":"SELECT id, name FROM public.customers","maxRows":1000}' --out imported.xlsx
gridcraft-cli run --in imported.xlsx --cmd 'data.refreshQuery={"table":"Query1"}' --out refreshed.xlsx
```

The CLI polls and applies these jobs before saving `--out`; previews return data directly.
Single jobs time out after five minutes; Refresh All allows time for its bounded query batch.
On timeout the CLI requests cancellation and returns an error. Avoid putting credentials in
shell history; prefer a DSN with suitable authentication or a private parameter source.

## Contributor checks

Portable unit tests cover conversion/limits; deterministic engine/UI/CLI tests cover load,
preview, refresh, cancellation, undo and credential redaction. The opt-in native integration
test requires an installed driver and a synthetic PostgreSQL database:

```powershell
$env:GRIDCRAFT_TEST_ODBC_DSN = 'GridCraftTest'
$env:GRIDCRAFT_TEST_ODBC_USER = 'readonly_test_user'
$env:GRIDCRAFT_TEST_ODBC_PASSWORD = '<test password>'
cargo test -p gridcraft-odbc --test postgres -- --ignored
```

`GRIDCRAFT_TEST_ODBC_CONNECTION_STRING` may replace the DSN. The integration test is ignored
by default and never connects to a business database automatically. Native connection behavior
still needs verification with a registered psqlODBC driver on the development machine.
