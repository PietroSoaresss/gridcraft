use std::{
    mem::size_of,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use gridcraft_core::Value;
use odbc_api::{
    ConnectionOptions, Cursor, DataType, Environment, ResultSetMetadata,
    buffers::Indicator,
    parameter::{VarCell, WideText},
};

use crate::{Column, ColumnKind, Connection as Settings, DataSet, Limits, OdbcError, Sources, decode};

fn env() -> Result<&'static Environment, OdbcError> {
    static ENV: OnceLock<Result<Environment, OdbcError>> = OnceLock::new();
    ENV.get_or_init(|| Environment::new().map_err(|_| OdbcError::Unavailable)).as_ref().map_err(Clone::clone)
}

pub fn sources() -> Result<Sources, OdbcError> {
    let env = env()?;
    let data_sources = env.data_sources().map_err(|_| OdbcError::Unavailable)?.into_iter().map(|s| s.server_name).collect();
    let drivers = env.drivers().map_err(|_| OdbcError::Unavailable)?.into_iter().map(|d| d.description).collect();
    Ok(Sources { data_sources, drivers })
}

fn connect<'a>(env: &'a Environment, settings: &Settings) -> Result<odbc_api::Connection<'a>, OdbcError> {
    let options = ConnectionOptions { login_timeout_sec: Some(15), ..Default::default() };
    env.connect_with_connection_string(&connection_string(settings)?, options).map_err(driver_error)
}

fn driver_error(error: odbc_api::Error) -> OdbcError {
    if let odbc_api::Error::Diagnostics { record, .. } = error
        && record.state.0.iter().all(u8::is_ascii_alphanumeric)
        && let Ok(state) = std::str::from_utf8(&record.state.0)
    {
        return OdbcError::DriverState(state.into());
    }
    OdbcError::QueryFailed
}

fn connection_string(settings: &Settings) -> Result<String, OdbcError> {
    settings.validate()?;
    let attribute = |value: &str| format!("{{{}}}", value.replace('}', "}}"));
    let base = match (&settings.dsn, &settings.connection_string) {
        (Some(dsn), None) => format!("DSN={dsn};"),
        (None, Some(cs)) => cs.clone(),
        _ => return Err(OdbcError::InvalidConnection),
    };
    // The manager favors first occurrences; psqlODBC processes options in order and favors last.
    let safety =
        "BoolsAsChar=0;ReadOnly=1;CommLog=0;Debug=0;ConnSettings={};UseDeclareFetch=1;Fetch=100;BI=-5;NumericAS=2;LFConversion=0;IgnoreTimeout=0;";
    let mut credentials = String::new();
    if !settings.user.is_empty() {
        credentials.push_str(&format!("UID={};", attribute(&settings.user)));
    }
    if !settings.password.is_empty() {
        credentials.push_str(&format!("PWD={};", attribute(&settings.password)));
    }
    let mut value = format!("{safety}{credentials}");
    value.push_str(&base);
    if !value.ends_with(';') {
        value.push(';');
    }
    value.push_str(&credentials);
    value.push_str(safety);
    Ok(value)
}

pub fn test(settings: &Settings) -> Result<(), OdbcError> {
    let connection = connect(env()?, settings)?;
    if !connection.database_management_system_name().map_err(driver_error)?.to_ascii_lowercase().contains("postgresql") {
        return Err(OdbcError::UnsupportedDatabase);
    }
    connection.execute("SELECT 1", (), Some(15)).map(|_| ()).map_err(driver_error)
}

pub fn query(settings: &Settings, sql: &str, limits: Limits, preview: bool, cancelled: &AtomicBool) -> Result<DataSet, OdbcError> {
    let limits = limits.validate()?;
    crate::validate_query(sql)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(OdbcError::Cancelled);
    }
    let connection = connect(env()?, settings)?;
    let dbms = connection.database_management_system_name().map_err(|_| OdbcError::ConnectionFailed)?;
    if !dbms.to_ascii_lowercase().contains("postgresql") {
        return Err(OdbcError::UnsupportedDatabase);
    }
    connection.set_autocommit(false).map_err(driver_error)?;
    connection.execute("SET TRANSACTION READ ONLY", (), Some(60)).map_err(driver_error)?;
    // psqlODBC uses cursor fetching for SELECT/WITH, but not bare TABLE/VALUES statements.
    let sql = sql.trim().strip_suffix(';').unwrap_or(sql.trim());
    let sql = format!("SELECT * FROM ({sql}\n) AS gridcraft_odbc_result");
    let mut cursor = connection.execute(&sql, (), Some(60)).map_err(driver_error)?.ok_or(OdbcError::InvalidData)?;
    let count = cursor.num_result_cols().map_err(|_| OdbcError::InvalidData)?;
    if count <= 0 || count as usize > limits.columns {
        return Err(OdbcError::Limit("The result has too many columns."));
    }
    let mut columns = Vec::with_capacity(count as usize);
    for i in 1..=count as u16 {
        let kind = match cursor.col_data_type(i).map_err(|_| OdbcError::InvalidData)? {
            DataType::Integer | DataType::SmallInt | DataType::BigInt | DataType::TinyInt => ColumnKind::Integer,
            DataType::Numeric { .. } | DataType::Decimal { .. } => ColumnKind::Decimal,
            DataType::Float { .. } | DataType::Real | DataType::Double => ColumnKind::Float,
            DataType::Bit => ColumnKind::Boolean,
            DataType::Date | DataType::Time { .. } | DataType::Timestamp { .. } => ColumnKind::Temporal,
            DataType::Char { .. }
            | DataType::WChar { .. }
            | DataType::Varchar { .. }
            | DataType::WVarchar { .. }
            | DataType::LongVarchar { .. }
            | DataType::WLongVarchar { .. } => ColumnKind::Text,
            _ => return Err(OdbcError::InvalidData),
        };
        let name = cursor.col_name(i).map_err(|_| OdbcError::InvalidData)?;
        if name.len() > 1024 || name.contains('\0') {
            return Err(OdbcError::InvalidData);
        }
        columns.push(Column { name, kind });
    }
    let mut rows = Vec::new();
    let max_rows = if preview { limits.rows.min(100) } else { limits.rows };
    let mut bytes = columns.iter().map(|c| c.name.len() + size_of::<Column>()).sum::<usize>();
    let mut truncated = false;
    let mut buffer = vec![0u16; limits.cell_bytes.saturating_add(1)];
    while let Some(mut row) = cursor.next_row().map_err(|_| OdbcError::InvalidData)? {
        if cancelled.load(Ordering::Relaxed) {
            return Err(OdbcError::Cancelled);
        }
        if rows.len() == max_rows {
            if !preview {
                return Err(OdbcError::Limit("The result exceeds the row limit. Restrict the query or increase maxRows."));
            }
            truncated = true;
            break;
        }
        bytes = bytes
            .checked_add(size_of::<Vec<Value>>() + columns.len() * size_of::<Value>())
            .ok_or(OdbcError::Limit("The result exceeds the supported size."))?;
        if bytes > limits.bytes {
            return Err(OdbcError::Limit("The result exceeds the supported size."));
        }
        let mut values = Vec::with_capacity(columns.len());
        for (i, column) in columns.iter().enumerate() {
            let mut cell = VarCell::<&mut [u16], WideText>::from_buffer(buffer.as_mut_slice(), Indicator::Null);
            row.get_data((i + 1) as u16, &mut cell).map_err(|_| OdbcError::InvalidData)?;
            if !cell.is_complete() {
                return Err(OdbcError::Limit("A result cell exceeds the supported size."));
            }
            let value = match cell.as_slice() {
                None => Value::Empty,
                Some(raw) => {
                    let text = String::from_utf16(raw).map_err(|_| OdbcError::InvalidData)?;
                    if text.len() > limits.cell_bytes {
                        return Err(OdbcError::Limit("A result cell exceeds the supported size."));
                    }
                    decode(column.kind, &text)?
                }
            };
            if let Value::Text(text) = &value {
                bytes = bytes.checked_add(text.len() + 2 * size_of::<usize>()).ok_or(OdbcError::Limit("The result exceeds the supported size."))?;
            }
            if bytes > limits.bytes {
                return Err(OdbcError::Limit("The result exceeds the supported size."));
            }
            values.push(value);
        }
        rows.push(values);
    }
    let dataset = DataSet { columns, rows, truncated };
    drop(cursor);
    connection.rollback().map_err(|_| OdbcError::QueryFailed)?;
    dataset.validate(limits)?;
    Ok(dataset)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_escape_braces_and_separators_without_quoting_the_dsn() {
        let settings = Settings { dsn: Some("Postgres".into()), user: "reader".into(), password: "{pass};+".into(), connection_string: None };
        let value = connection_string(&settings).unwrap();
        assert!(value.contains("UID={reader};PWD={{pass}};+};DSN=Postgres;"));
        assert!(value.starts_with("BoolsAsChar=0;ReadOnly=1;CommLog=0;Debug=0;ConnSettings={};"));
        let unsafe_settings = Settings {
            connection_string: Some("Driver={PostgreSQL Unicode};CX=ff;A0=0;B2=1;B3=1;A6={DELETE FROM x};B6=0;BI=8;D6=8".into()),
            ..Default::default()
        };
        let value = connection_string(&unsafe_settings).unwrap();
        assert!(value.contains("D6=8;BoolsAsChar=0;ReadOnly=1;CommLog=0;Debug=0;ConnSettings={};"));
        assert!(value.ends_with("UseDeclareFetch=1;Fetch=100;BI=-5;NumericAS=2;LFConversion=0;IgnoreTimeout=0;"));
    }
}
