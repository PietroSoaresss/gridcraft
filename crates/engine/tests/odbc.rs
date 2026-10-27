#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::disallowed_methods)] // Native worker deadlines, never executed on WASM.

use gridcraft_engine::Session;
use gridcraft_engine::{
    core::{CellRef, Value},
    odbc::OdbcManager,
};
use gridcraft_odbc::{Backend, Column, ColumnKind, Connection, DataSet, Limits, OdbcError, Sources, Value as Input};
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct Database {
    data: Mutex<DataSet>,
    blocked: AtomicBool,
    entered: AtomicBool,
}
impl Backend for Database {
    fn sources(&self) -> Result<Sources, OdbcError> {
        Ok(Sources { data_sources: vec!["Postgres".into()], drivers: vec!["PostgreSQL Unicode".into()] })
    }
    fn test(&self, _: &Connection) -> Result<(), OdbcError> {
        Ok(())
    }
    fn query(&self, connection: &Connection, sql: &str, _: Limits, _: bool, _: &AtomicBool) -> Result<DataSet, OdbcError> {
        self.entered.store(true, Ordering::Release);
        while self.blocked.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(2));
        }
        if sql == "SELECT failure" {
            return Err(OdbcError::QueryFailed);
        }
        if connection.dsn.as_deref() == Some("Another") {
            return Ok(data(vec![vec![Input::text("other database")]]));
        }
        Ok(self.data.lock().unwrap().clone())
    }
}

fn data(rows: Vec<Vec<Input>>) -> DataSet {
    DataSet { columns: vec![Column { name: "Value".into(), kind: ColumnKind::Text }], rows, truncated: false }
}

fn setup(rows: Vec<Vec<Input>>) -> (Session, Arc<Database>) {
    let database = Arc::new(Database { data: Mutex::new(data(rows)), blocked: AtomicBool::new(false), entered: AtomicBool::new(false) });
    let mut session = Session::new();
    session.odbc = OdbcManager::new(database.clone());
    session.new_workbook();
    (session, database)
}

fn wait(session: &mut Session, id: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = session.execute("data.odbcStatus", json!({"jobId": id})).unwrap();
        if status["state"] != "running" {
            return status;
        }
        assert!(Instant::now() < deadline, "ODBC worker did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn query(session: &mut Session, command: &str, params: serde_json::Value) -> String {
    let result = session.execute(command, params).unwrap();
    let id = result["jobId"].as_str().unwrap().to_string();
    assert_eq!(wait(session, &id)["state"], "ready");
    id
}

fn load(session: &mut Session) {
    let id = query(session, "data.fromOdbc", json!({"dsn":"Postgres", "sql":"SELECT values"}));
    session.execute("data.odbcApply", json!({"jobId":id})).unwrap();
}

#[test]
fn odbc_load_is_literal_atomic_undoable_and_survives_xlsx() {
    let (mut session, _) = setup(vec![vec![Input::text("=SUM(A1:A2)")], vec![Input::text("0012")], vec![Input::Empty]]);
    let before = session.doc().unwrap().wb.clone();
    let id = query(&mut session, "data.fromOdbc", json!({"connectionString":"Driver={example};PWD=do-not-save", "sql":"SELECT values"}));
    assert!(Arc::ptr_eq(&before, &session.doc().unwrap().wb));
    session.execute("data.odbcApply", json!({"jobId":id})).unwrap();
    let imported = session.doc().unwrap().wb.clone();
    assert_eq!(session.doc().unwrap().undo.len(), 1);
    let sheet = imported.active().unwrap();
    assert_eq!(sheet.cell(CellRef::new(1, 0)).unwrap().value, Value::text("=SUM(A1:A2)"));
    assert!(sheet.cell(CellRef::new(1, 0)).unwrap().formula.is_none());
    assert!(!serde_json::to_string(&*imported).unwrap().contains("do-not-save"));
    let (reopened, _) = gridcraft_xlsx::read_xlsx(&gridcraft_xlsx::write_xlsx(&imported).unwrap()).unwrap();
    assert_eq!(reopened.active().unwrap().tables.len(), 1);
    let model = serde_json::to_string(&*imported).unwrap();
    for secret in ["SELECT values", "do-not-save", "connectionRef", "gc:query"] {
        assert!(!model.contains(secret));
    }
    let bytes = gridcraft_xlsx::write_xlsx(&imported).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    for i in 0..archive.len() {
        let mut part = archive.by_index(i).unwrap();
        if part.name().ends_with(".xml") {
            use std::io::Read;
            let mut xml = String::new();
            part.read_to_string(&mut xml).unwrap();
            for secret in ["SELECT values", "do-not-save", "connectionRef", "gc:query"] {
                assert!(!xml.contains(secret), "{}", part.name());
            }
        }
    }
    assert_eq!(reopened.active().unwrap().cell(CellRef::new(1, 0)).unwrap().value, Value::text("=SUM(A1:A2)"));
    session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*session.doc().unwrap().wb, *before);
    session.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*session.doc().unwrap().wb, *imported);
}

#[test]
fn odbc_empty_failure_and_stale_results_preserve_the_workbook() {
    let (mut session, _) = setup(vec![]);
    load(&mut session);
    assert!(session.doc().unwrap().wb.active().unwrap().tables[0].data_range().is_none());
    let before = session.doc().unwrap().wb.clone();
    let result = session.execute("data.fromOdbc", json!({"dsn":"Postgres", "sql":"SELECT failure"})).unwrap();
    assert_eq!(wait(&mut session, result["jobId"].as_str().unwrap())["state"], "failed");
    assert!(Arc::ptr_eq(&before, &session.doc().unwrap().wb));
    let id = query(&mut session, "data.fromOdbc", json!({"dsn":"Postgres", "sql":"SELECT values"}));
    session.execute("cell.set", json!({"cell":"C1", "input":"changed"})).unwrap();
    assert!(session.execute("data.odbcApply", json!({"jobId":id})).is_err());
    session.new_workbook();
    assert!(session.execute("data.odbcApply", json!({"jobId":id})).is_err());
    assert_eq!(session.doc().unwrap().wb.sheets.len(), 1);
}

#[test]
fn odbc_cancel_keeps_a_busy_driver_accounted_for_until_it_returns() {
    let (mut session, database) = setup(vec![]);
    database.blocked.store(true, Ordering::Release);
    let result = session.execute("data.fromOdbc", json!({"dsn":"Postgres", "sql":"SELECT values"})).unwrap();
    let id = result["jobId"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !database.entered.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    session.execute("data.odbcCancel", json!({"jobId":id})).unwrap();
    let busy = session.execute("data.fromOdbc", json!({"dsn":"Postgres", "sql":"SELECT values"}));
    database.blocked.store(false, Ordering::Release);
    assert!(busy.is_err());
    assert_eq!(wait(&mut session, id)["state"], "cancelled");
    assert!(session.execute("data.odbcApply", json!({"jobId":id})).is_err());
    assert_eq!(session.doc().unwrap().wb.sheets.len(), 1);
}

#[test]
fn odbc_commands_are_registered_and_validate_before_starting_work() {
    let mut session = Session::new();
    session.new_workbook();
    for command in ["data.odbcTest", "data.odbcPreview", "data.fromOdbc", "data.odbcStatus", "data.odbcApply", "data.odbcCancel"] {
        let error = session.execute(command, json!({})).expect_err("missing parameters");
        assert!(!matches!(error, gridcraft_engine::EngineError::UnknownCommand(_)), "{command}: {error}");
    }
}

#[test]
fn odbc_dates_use_the_workbook_epoch_and_survive_xlsx_as_dates() {
    use gridcraft_odbc::decode;
    for system in [gridcraft_engine::core::DateSystem::D1900, gridcraft_engine::core::DateSystem::D1904] {
        let (mut session, database) = setup(vec![]);
        Arc::make_mut(&mut session.doc_mut().unwrap().wb).date_system = system;
        *database.data.lock().unwrap() = DataSet {
            columns: vec![
                Column { name: "Date".into(), kind: ColumnKind::Date },
                Column { name: "Timestamp".into(), kind: ColumnKind::Timestamp },
                Column { name: "Offset".into(), kind: ColumnKind::TimestampWithOffset },
                Column { name: "Historical".into(), kind: ColumnKind::Date },
                Column { name: "BeforeEpoch".into(), kind: ColumnKind::Timestamp },
            ],
            rows: vec![vec![
                decode(ColumnKind::Date, "2024-02-29").unwrap(),
                decode(ColumnKind::Timestamp, "2024-02-29 12:00:00.123456").unwrap(),
                decode(ColumnKind::TimestampWithOffset, "2024-02-29 12:00:00 +03:00").unwrap(),
                decode(ColumnKind::Date, "1800-01-01").unwrap(),
                decode(
                    ColumnKind::Timestamp,
                    if system == gridcraft_engine::core::DateSystem::D1900 { "1899-12-31 12:00:00" } else { "1903-12-31 12:00:00" },
                )
                .unwrap(),
            ]],
            truncated: false,
        };
        let id = query(&mut session, "data.fromOdbc", json!({"dsn":"Postgres", "sql":"SELECT dates"}));
        session.execute("data.odbcApply", json!({"jobId":id})).unwrap();
        let wb = &session.doc().unwrap().wb;
        let sheet = wb.active().unwrap();
        let serial = gridcraft_engine::core::date::serial_from_ymd(system, 2024, 2, 29).unwrap();
        assert_eq!(sheet.cell(CellRef::new(1, 0)).unwrap().value, Value::Number(serial));
        assert_eq!(wb.styles.get(sheet.cell(CellRef::new(1, 0)).unwrap().style).num_fmt.as_str(), "yyyy-mm-dd");
        let Value::Number(timestamp) = sheet.cell(CellRef::new(1, 1)).unwrap().value else { panic!("expected date serial") };
        assert!((timestamp - serial - 0.5 - 0.123456 / 86400.0).abs() < 1e-10);
        assert_eq!(sheet.cell(CellRef::new(1, 2)).unwrap().value, Value::text("2024-02-29T12:00:00+03:00"));
        assert_eq!(sheet.cell(CellRef::new(1, 3)).unwrap().value, Value::text("1800-01-01"));
        assert_eq!(
            sheet.cell(CellRef::new(1, 4)).unwrap().value,
            Value::text(if system == gridcraft_engine::core::DateSystem::D1900 { "1899-12-31 12:00:00" } else { "1903-12-31 12:00:00" })
        );
        let (reopened, _) = gridcraft_xlsx::read_xlsx(&gridcraft_xlsx::write_xlsx(wb).unwrap()).unwrap();
        assert_eq!(reopened.date_system, system);
        let cell = reopened.active().unwrap().cell(CellRef::new(1, 0)).unwrap();
        assert_eq!(cell.value, Value::Number(serial));
        assert_eq!(reopened.styles.get(cell.style).num_fmt.as_str(), "yyyy-mm-dd");
    }
}

#[test]
fn odbc_preview_test_and_refresh_all_do_not_create_saved_queries() {
    let (mut session, _) = setup(vec![vec![Input::text("one")]]);
    let before = session.doc().unwrap().wb.clone();
    let test = session.execute("data.odbcTest", json!({"dsn":"Postgres"})).unwrap();
    assert_eq!(wait(&mut session, test["jobId"].as_str().unwrap())["state"], "completed");
    let id = query(&mut session, "data.odbcPreview", json!({"dsn":"Postgres", "sql":"SELECT values"}));
    let preview = session.execute("data.odbcApply", json!({"jobId":id})).unwrap();
    assert_eq!(serde_json::from_value::<Value>(preview["rows"][0][0].clone()).unwrap(), Value::text("one"));
    assert!(Arc::ptr_eq(&before, &session.doc().unwrap().wb));
    let result = session.execute("data.refreshAll", json!({})).unwrap();
    assert!(result.get("jobId").is_none());
    assert!(matches!(session.execute("data.refreshQuery", json!({})), Err(gridcraft_engine::EngineError::UnknownCommand(_))));
}

#[test]
fn odbc_secrets_are_removed_recursively_from_recorded_parameters() {
    let recorded = gridcraft_engine::cmd::journal_params(&json!({
        "sql": "SELECT 1", "connectionString": "Driver=example;PWD=top-secret",
        "connection": {"dsn": "Example", "password": "top-secret", "nested": [{"apiPassword": "top-secret"}]}
    }));
    assert!(!recorded.to_string().contains("top-secret"));
    assert_eq!(recorded["sql"], "SELECT 1");
    assert_eq!(recorded["connection"]["dsn"], "Example");
}

#[test]
fn recorded_parameter_redaction_has_a_depth_limit() {
    let mut input = json!({"PWD":"do-not-save"});
    for _ in 0..100 {
        input = json!([input]);
    }
    let recorded = gridcraft_engine::cmd::journal_params(&input).to_string();
    assert!(!recorded.contains("do-not-save"));
    assert!(recorded.contains("null"));
}
