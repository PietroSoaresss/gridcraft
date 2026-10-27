# gridcraft-odbc

Bounded, read-only ODBC queries independent of the spreadsheet engine and UI.
The current native implementation supports Windows drivers for PostgreSQL,
SQL Server/Azure SQL, MySQL, MariaDB, Oracle and SQLite 3. Other targets build
the same API and return `OdbcError::Unavailable` for native operations.

```rust,no_run
use gridcraft_odbc::{Backend, Connection, Limits, NativeBackend};
use std::sync::atomic::AtomicBool;

# fn main() -> Result<(), gridcraft_odbc::OdbcError> {
let connection = Connection { dsn: Some("Reporting".into()), ..Default::default() };
let data = NativeBackend.query(
    &connection, "SELECT * FROM customers", Limits::default(), false,
    &AtomicBool::new(false),
)?;
for column in &data.columns {
    println!("{}: {:?}", column.name, column.kind);
}
# Ok(())
# }
```

## Contract

`Backend::query` returns a `DataSet` containing column names/types, row values and
a preview truncation flag. NULL is `Value::Empty`. Numbers above spreadsheet
precision remain exact text. Text is never interpreted as a formula. Dates,
times and timestamps use Chrono types, retaining nanoseconds and explicit
offsets when supplied by ODBC metadata. No workbook epoch is chosen here.
Timestamp columns whose driver omits timezone information remain timezone-free.
Nonstandard temporal strings (including infinite dates, zero dates and duration
values outside a time of day) return `InvalidData`; explicitly cast them to text.
SQLite expressions with unknown metadata remain text rather than guessed dates.

The result is materialized and bounded: 100,000 rows, 256 columns, 128 MiB
including container overhead, and 1 MiB per text cell. Preview reads at most
100 rows and may return `truncated`; full queries fail on overflow. Cancellation
is checked between native reads. A blocking driver call may finish first.
Arrow batches and unixODBC are follow-ups; this crate does not define saved
queries, refresh or an XLSX extension. See upstream issue #431 for that model.

## Drivers and safety

Install a matching 64-bit ODBC driver and configure a DSN, or supply a registered
driver connection string. Use separate user/password fields. Driver detection,
connection options, query guards and transaction behavior live in
`src/native/profile.rs`; values and bounded fetching live in `src/native.rs`.
Credentials and driver diagnostics are redacted from public errors and Debug.
Disable ODBC manager/vendor tracing separately.

Queries accept a single SELECT/WITH/VALUES/TABLE statement. Write/control
keywords, executable comments and internal semicolons are rejected. Connections
roll back after reading. PostgreSQL/Oracle use a server read-only transaction;
MySQL/MariaDB start a read-only transaction; SQLite enables `query_only`.
SQL Server read intent is a routing hint: a SELECT-only account is required.
Query guards do not sandbox database functions or replace server permissions.

## Validation and contributions

Run `cargo test -p gridcraft-odbc`. Synthetic integration tests are ignored by
default and need dedicated local databases plus installed drivers:

```powershell
$env:GRIDCRAFT_TEST_ODBC_DSN = 'GridCraftTest'
$env:GRIDCRAFT_TEST_ODBC_USER = 'reader'
$env:GRIDCRAFT_TEST_ODBC_PASSWORD = '<local test password>'
cargo test -p gridcraft-odbc --test postgres -- --ignored
```

`GRIDCRAFT_TEST_ODBC_CONNECTION_STRING` replaces the DSN. Other databases use
`GRIDCRAFT_TEST_ODBC_{SQLSERVER,MYSQL,MARIADB,ORACLE,SQLITE}_{DSN,CONNECTION_STRING,USER,PASSWORD}`
and `cargo test -p gridcraft-odbc --test databases -- --ignored` (or a test-name
filter). SQLite requires an existing synthetic file. These tests create no
server objects. Add a driver profile and synthetic integration case when adding
a database family. Keep vendor settings out of engine/UI code and keep native
dependencies out of WASM.
