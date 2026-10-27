#![cfg(target_os = "windows")]
//! Each ignored test needs its own dedicated database/driver; no server objects are created.
use gridcraft_odbc::{Backend, ColumnKind, Connection, Limits, NativeBackend, OdbcError, Value, decode};
use std::sync::atomic::AtomicBool;

fn read_database(name: &str, sql: &str, oracle: bool) {
    let prefix = format!("GRIDCRAFT_TEST_ODBC_{name}");
    let dsn = std::env::var(format!("{prefix}_DSN")).ok();
    let connection = Connection {
        connection_string: if dsn.is_some() {
            None
        } else {
            Some(std::env::var(format!("{prefix}_CONNECTION_STRING")).expect("dedicated test connection string"))
        },
        dsn,
        user: std::env::var(format!("{prefix}_USER")).unwrap_or_default(),
        password: std::env::var(format!("{prefix}_PASSWORD")).unwrap_or_default(),
    };
    let backend = NativeBackend;
    backend.test(&connection).unwrap();
    let cancel = AtomicBool::new(false);
    let data = backend.query(&connection, sql, Limits::default(), false, &cancel).unwrap();
    assert_eq!(data.rows.len(), 1);
    let expected_number =
        if name == "SQLITE" && data.columns[0].kind == gridcraft_odbc::ColumnKind::Text { Value::text("42") } else { Value::Number(42.0) };
    assert_eq!(data.rows[0][0], expected_number);
    assert_eq!(data.rows[0][1], Value::text("=1"));
    assert_eq!(data.rows[0][2], Value::text("ação 🦊"));
    assert_eq!(data.rows[0][3], Value::text("9007199254740993"));
    assert_eq!(data.rows[0][4], Value::Empty);
    let temporal_sql = match name {
        "SQLSERVER" => Some(
            "SELECT CAST('2024-02-29' AS date), CAST('12:34:56.123456' AS time(6)), CAST('2024-02-29 12:34:56.123456' AS datetime2(6)), CAST('2024-02-29 12:34:56.123456 +03:00' AS datetimeoffset(6))",
        ),
        "MYSQL" | "MARIADB" => {
            Some("SELECT CAST('2024-02-29' AS DATE), CAST('12:34:56.123456' AS TIME(6)), CAST('2024-02-29 12:34:56.123456' AS DATETIME(6))")
        }
        "ORACLE" => Some("SELECT TIMESTAMP '2024-02-29 12:34:56.123456' FROM DUAL"),
        _ => None,
    };
    if let Some(sql) = temporal_sql {
        let dates = backend.query(&connection, sql, Limits::default(), false, &cancel).unwrap();
        let timestamp = decode(ColumnKind::Timestamp, "2024-02-29 12:34:56.123456").unwrap();
        if oracle {
            assert_eq!(dates.rows[0][0], timestamp);
        } else {
            assert_eq!(dates.rows[0][0], decode(ColumnKind::Date, "2024-02-29").unwrap());
            assert_eq!(dates.rows[0][1], decode(ColumnKind::Time, "12:34:56.123456").unwrap());
            assert_eq!(dates.rows[0][2], timestamp);
            if name == "SQLSERVER" {
                assert_eq!(dates.rows[0][3], decode(ColumnKind::TimestampWithOffset, "2024-02-29 12:34:56.123456 +03:00").unwrap());
            }
        }
    }
    let from = if oracle { " FROM DUAL" } else { "" };
    let two_rows = format!("SELECT 1 AS value{from} UNION ALL SELECT 2 AS value{from}");
    let preview = backend.query(&connection, &two_rows, Limits { rows: 1, ..Default::default() }, true, &cancel).unwrap();
    assert_eq!(preview.rows.len(), 1);
    assert!(preview.truncated);
    assert!(matches!(backend.query(&connection, &two_rows, Limits { rows: 1, ..Default::default() }, false, &cancel), Err(OdbcError::Limit(_))));
    let empty = format!("SELECT 1 AS value{from} WHERE 1=0");
    assert!(backend.query(&connection, &empty, Limits::default(), false, &cancel).unwrap().rows.is_empty());
    assert!(matches!(backend.query(&connection, sql, Limits::default(), false, &AtomicBool::new(true)), Err(OdbcError::Cancelled)));
}

#[test]
#[ignore = "requires a dedicated SQLSERVER ODBC test connection"]
fn sql_server_reads_unicode_literals_and_exact_bigints() {
    read_database(
        "SQLSERVER",
        "SELECT CAST(42 AS int) AS n, CAST('=1' AS nvarchar(10)) AS literal, N'ação 🦊' AS unicode_text, CAST(9007199254740993 AS bigint) AS identifier, CAST(NULL AS nvarchar(10)) AS optional_text",
        false,
    );
}

#[test]
#[ignore = "requires a dedicated MYSQL ODBC test connection"]
fn mysql_reads_unicode_literals_and_exact_bigints() {
    read_database(
        "MYSQL",
        "SELECT CAST(42 AS SIGNED) AS n, '=1' AS literal, 'ação 🦊' AS unicode_text, CAST(9007199254740993 AS SIGNED) AS identifier, NULL AS optional_text",
        false,
    );
}

#[test]
#[ignore = "requires a dedicated MARIADB ODBC test connection"]
fn mariadb_reads_unicode_literals_and_exact_bigints() {
    read_database(
        "MARIADB",
        "SELECT CAST(42 AS SIGNED) AS n, '=1' AS literal, 'ação 🦊' AS unicode_text, CAST(9007199254740993 AS SIGNED) AS identifier, NULL AS optional_text",
        false,
    );
}

#[test]
#[ignore = "requires a dedicated ORACLE ODBC test connection"]
fn oracle_reads_unicode_literals_and_exact_numbers() {
    read_database(
        "ORACLE",
        "SELECT CAST(42 AS NUMBER(10)) AS n, '=1' AS literal, N'ação 🦊' AS unicode_text, CAST(9007199254740993 AS NUMBER(19)) AS identifier, CAST(NULL AS VARCHAR2(10)) AS optional_text FROM DUAL",
        true,
    );
}

#[test]
#[ignore = "requires a dedicated SQLITE ODBC test connection and existing database"]
fn sqlite_reads_unicode_literals_and_exact_integers() {
    read_database(
        "SQLITE",
        "SELECT CAST(42 AS INTEGER) AS n, '=1' AS literal, 'ação 🦊' AS unicode_text, CAST(9007199254740993 AS INTEGER) AS identifier, CAST(NULL AS TEXT) AS optional_text",
        false,
    );
}
