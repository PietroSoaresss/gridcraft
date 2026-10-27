use super::*;
use crate::odbc::Request;
use gridcraft_core::{CellRef, MAX_COLS, MAX_ROWS, RangeRef, Value};
use gridcraft_model::{Sheet, Table, TableColumn, TotalsFn};
use gridcraft_odbc::{Connection, DataSet, Limits};
use serde_json::{Value as Json, json};
use std::sync::Arc;

fn other(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

pub fn specs() -> Vec<CommandSpec> {
    let mut apply = cmd!(query "data.odbcApply", "Apply ODBC Results", [], None, "{jobId} → one undoable edit", has_doc, apply);
    apply.undoable = true;
    vec![
        cmd!(query "data.odbcSources", "ODBC Sources", ["Data", "Get Data"], None, "{} → {dataSources, drivers}", always, sources),
        cmd!(query "data.odbcTest", "Test ODBC Connection", ["Data", "Get Data"], None, "{dsn | connectionString, user?, password?} → {jobId}", has_doc, |s, p| submit(s, p, "test")),
        cmd!(query "data.odbcPreview", "Preview ODBC Query", ["Data", "Get Data"], None, "{dsn | connectionString, user?, password?, sql} → {jobId}", has_doc, |s, p| submit(s, p, "preview")),
        cmd!(query "data.fromOdbc", "Load ODBC Query", ["Data", "Get Data"], None, "{dsn | connectionString, user?, password?, sql, maxRows?: 1..100000} → {jobId}", has_doc, |s, p| submit(s, p, "load")),
        cmd!(query "data.odbcStatus", "ODBC Job Status", [], None, "{jobId} → {state, error?, rows?, columns?, truncated}", always, status),
        cmd!(query "data.odbcCancel", "Cancel ODBC Job", [], None, "{jobId}", always, cancel),
        apply,
    ]
}

fn sources(s: &mut Session, _: &Json) -> Result<Json> {
    serde_json::to_value(s.odbc.sources().map_err(|e| other(e.to_string()))?).map_err(|e| other(e.to_string()))
}

fn settings(p: &Json, mut base: Connection) -> Result<Connection> {
    for key in ["dsn", "connectionString", "user", "password"] {
        if p.get(key).is_some_and(|v| !v.is_string()) {
            return Err(bad("ODBC", format!("`{key}` must be text")));
        }
        let limit = match key {
            "dsn" => 255,
            "user" => 512,
            _ => 16384,
        };
        if str_param(p, key).is_some_and(|s| s.len() > limit) {
            return Err(bad("ODBC", format!("`{key}` is too long")));
        }
    }
    if p.get("dsn").is_some() || p.get("connectionString").is_some() {
        base.dsn = str_param(p, "dsn").map(str::to_string);
        base.connection_string = str_param(p, "connectionString").map(str::to_string);
    }
    if let Some(user) = str_param(p, "user") {
        base.user = user.into();
    }
    if let Some(password) = str_param(p, "password") {
        base.password = password.into();
    }
    base.validate().map_err(|e| bad("ODBC", e.to_string()))?;
    Ok(base)
}

fn limits(p: &Json, preview: bool) -> Result<Limits> {
    let mut value = Limits::default();
    if let Some(rows) = p.get("maxRows") {
        value.rows = rows.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad("ODBC", "`maxRows` must be a positive integer"))?;
    }
    value = value.validate().map_err(|e| bad("ODBC", e.to_string()))?;
    if preview {
        value.rows = value.rows.min(100);
        value.bytes = 2 * 1024 * 1024;
    }
    value.validate().map_err(|e| bad("ODBC", e.to_string()))
}

fn submit(s: &mut Session, p: &Json, kind: &str) -> Result<Json> {
    let doc = s.doc()?;
    let sql = if kind == "test" {
        String::new()
    } else {
        let sql = str_param(p, "sql").ok_or_else(|| bad("ODBC", "`sql` is required"))?;
        gridcraft_odbc::validate_query(sql).map_err(|e| bad("ODBC", e.to_string()))?;
        sql.to_string()
    };
    let request = Request {
        uid: doc.uid,
        revision: doc.revision,
        kind: kind.into(),
        connection: settings(p, Connection::default())?,
        sql,
        limits: limits(p, kind == "preview")?,
    };
    start(s, request)
}

fn start(s: &mut Session, request: Request) -> Result<Json> {
    let id = s.odbc.submit(request).map_err(other)?;
    Ok(json!({"jobId": id, "state": "running"}))
}

fn job_id(p: &Json) -> Result<&str> {
    str_param(p, "jobId").filter(|s| s.len() <= 128).ok_or_else(|| bad("ODBC", "`jobId` is required"))
}

fn status(s: &mut Session, p: &Json) -> Result<Json> {
    let info = s.odbc.status(job_id(p)?).ok_or_else(|| bad("ODBC", "ODBC job was not found."))?;
    serde_json::to_value(info).map_err(|e| other(e.to_string()))
}

fn cancel(s: &mut Session, p: &Json) -> Result<Json> {
    s.odbc.cancel(job_id(p)?).map_err(other)?;
    Ok(json!({"cancellationRequested": true}))
}

fn apply(s: &mut Session, p: &Json) -> Result<Json> {
    let id = job_id(p)?.to_string();
    let doc = s.doc()?;
    let (data, request) = s.odbc.ready(&id, doc.uid, doc.revision).map_err(other)?;
    let result = match request.kind.as_str() {
        "preview" => {
            let rows: Vec<Vec<Value>> = data
                .rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|v| {
                            let (value, format) = cell_value(v, doc.wb.date_system);
                            format
                                .map(|f| {
                                    Value::text(
                                        gridcraft_numfmt::format_value(&value, &gridcraft_numfmt::NumberFormat::parse(f), doc.wb.date_system).text,
                                    )
                                })
                                .unwrap_or(value)
                        })
                        .collect()
                })
                .collect();
            json!({"columns": data.columns, "rows": rows, "truncated": data.truncated})
        }
        "load" => edit(s, |cx| import(cx, &data))?,
        _ => return Err(bad("ODBC", "This job has no data to apply.")),
    };
    s.odbc.complete(&id);
    Ok(result)
}

fn column_names(data: &DataSet) -> Vec<String> {
    let mut used = std::collections::HashSet::new();
    data.columns
        .iter()
        .enumerate()
        .map(|(i, column)| {
            let base = if column.name.trim().is_empty() { format!("Column{}", i + 1) } else { column.name.chars().take(240).collect::<String>() };
            let mut name = base.clone();
            let mut suffix = 2;
            while !used.insert(name.to_lowercase()) {
                name = format!("{base}_{suffix}");
                suffix += 1;
            }
            name
        })
        .collect()
}

fn result_range(start: CellRef, data: &DataSet) -> Result<RangeRef> {
    let row =
        start.row.checked_add(data.rows.len() as u32).filter(|r| *r < MAX_ROWS).ok_or_else(|| bad("ODBC", "Result does not fit in the sheet."))?;
    let col = data
        .columns
        .len()
        .checked_sub(1)
        .and_then(|n| start.col.checked_add(n as u32))
        .filter(|c| *c < MAX_COLS)
        .ok_or_else(|| bad("ODBC", "Result does not fit in the sheet."))?;
    Ok(RangeRef::new(start, CellRef::new(row, col)))
}

fn cell_value(value: &gridcraft_odbc::Value, system: gridcraft_core::DateSystem) -> (Value, Option<&'static str>) {
    use gridcraft_odbc::{Datelike, Timelike, Value as Input};
    let date = |d: gridcraft_odbc::NaiveDate| {
        let minimum = match system {
            gridcraft_core::DateSystem::D1900 => 1900,
            gridcraft_core::DateSystem::D1904 => 1904,
        };
        // DATE's 0..1899 year shorthand must never reinterpret a database year.
        (d.year() >= minimum).then(|| gridcraft_core::date::serial_from_ymd(system, d.year() as i64, d.month() as i64, d.day() as i64)).flatten()
    };
    let time = |t: gridcraft_odbc::NaiveTime| (t.num_seconds_from_midnight() as f64 + t.nanosecond() as f64 / 1e9) / 86400.0;
    match value {
        Input::Empty => (Value::Empty, None),
        Input::Bool(v) => (Value::Bool(*v), None),
        Input::Number(v) => (Value::Number(*v), None),
        Input::Text(v) => (Value::text(v.clone()), None),
        Input::Date(v) => date(*v).map(|n| (Value::Number(n), Some("yyyy-mm-dd"))).unwrap_or_else(|| (Value::text(v.to_string()), None)),
        Input::Time(v) if v.nanosecond() < 1_000_000_000 => (Value::Number(time(*v)), Some("hh:mm:ss.000")),
        Input::Time(v) => (Value::text(v.to_string()), None),
        Input::Timestamp(v) if v.nanosecond() < 1_000_000_000 => date(v.date())
            .map(|n| (Value::Number(n + time(v.time())), Some("yyyy-mm-dd hh:mm:ss.000")))
            .unwrap_or_else(|| (Value::text(v.to_string()), None)),
        Input::Timestamp(v) => (Value::text(v.to_string()), None),
        // Spreadsheet serials have no timezone; retain the offset explicitly.
        Input::TimestampWithOffset(v) => (Value::text(v.to_rfc3339()), None),
    }
}

fn write_data(cx: &mut Ctx<'_>, sheet: &mut Sheet, range: RangeRef, data: &DataSet, names: &[String]) {
    for (col, name) in names.iter().enumerate() {
        sheet.set_value(CellRef::new(range.start.row, range.start.col + col as u32), Value::text(name.clone()));
    }
    for (row, values) in data.rows.iter().enumerate() {
        for (col, value) in values.iter().enumerate() {
            let cell = CellRef::new(range.start.row + row as u32 + 1, range.start.col + col as u32);
            let (value, format) = cell_value(value, cx.wb.date_system);
            sheet.set_value(cell, value);
            if let Some(format) = format {
                let style = gridcraft_model::Style { num_fmt: gridcraft_model::NumFmt::new(format), ..Default::default() };
                let id = cx.wb.styles.intern(style);
                sheet.set_style(cell, id);
            }
        }
    }
}

fn import(cx: &mut Ctx<'_>, data: &DataSet) -> Result<Json> {
    if cx.wb.protected_structure {
        return Err(other("Workbook structure is protected."));
    }
    let range = result_range(CellRef::new(0, 0), data)?;
    let name = (1u32..).map(|n| format!("Query{n}")).find(|n| cx.wb.table(n).is_none()).ok_or_else(|| other("No query table name is available."))?;
    let mut sheet = Sheet::new(cx.wb.next_sheet_name());
    let names = column_names(data);
    write_data(cx, &mut sheet, range, data, &names);
    let table_id = cx
        .wb
        .sheets
        .iter()
        .flat_map(|s| &s.tables)
        .map(|t| t.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| other("Table identifiers are exhausted."))?;
    sheet.tables.push(Table {
        id: table_id,
        name: name.clone(),
        range,
        header_row: true,
        totals_row: false,
        columns: names
            .into_iter()
            .map(|name| TableColumn { name, totals: TotalsFn::None, totals_label: None, formula: None, unique_name: None, xml: None })
            .collect(),
        style: "TableStyleMedium2".into(),
        banded_rows: true,
        banded_cols: false,
        first_col: false,
        last_col: false,
        filter_button: true,
        filter: vec![],
        table_type: None,
        connection_id: None,
    });
    let sheet_name = sheet.name.clone();
    cx.wb.set_active_sheet(cx.wb.sheets.len());
    cx.wb.sheets.push(Arc::new(sheet));
    *cx.sel = Selection::at(range.start);
    cx.structural = true;
    Ok(json!({"table": name, "sheet": sheet_name, "range": range.a1(), "rows": data.rows.len(), "truncated": data.truncated}))
}
