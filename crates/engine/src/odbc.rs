//! Session-only ODBC workers. Workers never hold or mutate a workbook.
use gridcraft_odbc::{Backend, Connection, DataSet, Limits, NativeBackend, OdbcError};
use serde::Serialize;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Running,
    Ready,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: String,
    pub state: JobState,
    pub kind: String,
    pub error: Option<String>,
    pub rows: Option<usize>,
    pub columns: Option<usize>,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct Request {
    pub uid: u64,
    pub revision: u64,
    pub kind: String,
    pub connection: Connection,
    pub sql: String,
    pub limits: Limits,
}

struct Job {
    info: JobInfo,
    request: Request,
    data: Option<DataSet>,
    cancel: Arc<AtomicBool>,
}

pub struct OdbcManager {
    jobs: Arc<Mutex<Vec<Job>>>,
    backend: Arc<dyn Backend>,
    next_id: u64,
}

impl Default for OdbcManager {
    fn default() -> Self {
        Self::new(Arc::new(NativeBackend))
    }
}

impl OdbcManager {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { jobs: Arc::new(Mutex::new(Vec::new())), backend, next_id: 1 }
    }

    pub fn sources(&self) -> Result<gridcraft_odbc::Sources, OdbcError> {
        self.backend.sources()
    }

    pub fn submit(&mut self, request: Request) -> Result<String, String> {
        if cfg!(target_arch = "wasm32") {
            return Err(OdbcError::Unavailable.to_string());
        }
        request.limits.validate().map_err(|e| e.to_string())?;
        request.connection.validate().map_err(|e| e.to_string())?;
        if !matches!(request.kind.as_str(), "test" | "preview" | "load") {
            return Err("Invalid ODBC operation.".into());
        }
        if request.kind != "test" {
            gridcraft_odbc::validate_query(&request.sql).map_err(|e| e.to_string())?;
        }
        install_panic_redaction();
        let id = format!("odbc-{}", self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or("ODBC job identifiers are exhausted.")?;
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if jobs.iter().any(|j| matches!(j.info.state, JobState::Running | JobState::Ready)) {
                return Err("Finish or cancel the current ODBC job first.".into());
            }
            if jobs.len() >= 32 {
                jobs.remove(0);
            }
            jobs.push(Job {
                info: JobInfo {
                    id: id.clone(),
                    state: JobState::Running,
                    kind: request.kind.clone(),
                    error: None,
                    rows: None,
                    columns: None,
                    truncated: false,
                },
                request: request.clone(),
                data: None,
                cancel: cancel.clone(),
            });
        }
        let jobs = self.jobs.clone();
        let backend = self.backend.clone();
        let worker_id = id.clone();
        let spawn = thread::Builder::new().name("gridcraft-odbc".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if cancel.load(Ordering::Relaxed) {
                    return Err(OdbcError::Cancelled);
                }
                if request.kind == "test" {
                    backend.test(&request.connection).map(|_| None)
                } else {
                    let data = backend.query(&request.connection, &request.sql, request.limits, request.kind == "preview", &cancel)?;
                    if request.kind != "preview" && data.truncated {
                        return Err(OdbcError::Limit("The result exceeds the row limit."));
                    }
                    data.validate(request.limits)?;
                    Ok(Some(data))
                }
            }));
            let mut jobs = jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(job) = jobs.iter_mut().find(|j| j.info.id == worker_id) else { return };
            // Keep Running until the driver returns, even when cancellation was requested.
            if cancel.load(Ordering::Relaxed) {
                job.info.state = JobState::Cancelled;
            } else {
                match result {
                    Ok(Ok(Some(data))) => {
                        job.info.rows = Some(data.rows.len());
                        job.info.columns = Some(data.columns.len());
                        job.info.truncated = data.truncated;
                        job.data = Some(data);
                        job.info.state = JobState::Ready;
                    }
                    Ok(Ok(None)) => job.info.state = JobState::Completed,
                    Ok(Err(OdbcError::Cancelled)) => job.info.state = JobState::Cancelled,
                    Ok(Err(error)) => {
                        job.info.state = JobState::Failed;
                        job.info.error = Some(error.to_string());
                    }
                    Err(_) => {
                        job.info.state = JobState::Failed;
                        job.info.error = Some("The ODBC driver operation failed.".into());
                    }
                }
            }
            clear_request(&mut job.request);
        });
        if spawn.is_err() {
            self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|j| j.info.id != id);
            return Err("Could not start the ODBC worker.".into());
        }
        Ok(id)
    }

    pub fn status(&self, id: &str) -> Option<JobInfo> {
        self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|j| j.info.id == id).map(|j| j.info.clone())
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let mut jobs = self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let job = jobs.iter_mut().find(|j| j.info.id == id).ok_or("ODBC job was not found.")?;
        job.cancel.store(true, Ordering::Relaxed);
        if job.info.state == JobState::Ready {
            job.info.state = JobState::Cancelled;
            job.data = None;
            clear_request(&mut job.request);
        }
        Ok(())
    }

    pub fn ready(&self, id: &str, uid: u64, revision: u64) -> Result<(DataSet, Request), String> {
        let jobs = self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let job = jobs.iter().find(|j| j.info.id == id).ok_or("ODBC job was not found.")?;
        if job.info.state != JobState::Ready || job.cancel.load(Ordering::Relaxed) {
            return Err("ODBC job is not ready.".into());
        }
        if job.request.uid != uid || job.request.revision != revision {
            return Err("The workbook changed while querying; rerun the query.".into());
        }
        let data = job.data.clone().ok_or("ODBC job has no result.")?;
        Ok((data, job.request.clone()))
    }

    pub fn complete(&mut self, id: &str) {
        let mut jobs = self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(job) = jobs.iter_mut().find(|j| j.info.id == id) {
            job.info.state = JobState::Completed;
            job.data = None;
            clear_request(&mut job.request);
        }
    }

    pub fn discard_document(&mut self, uid: u64) {
        let mut jobs = self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for job in jobs.iter_mut().filter(|j| j.request.uid == uid) {
            job.cancel.store(true, Ordering::Relaxed);
            job.data = None;
            if job.info.state != JobState::Running {
                job.info.state = JobState::Cancelled;
            }
            clear_request(&mut job.request);
        }
    }
}

fn clear_request(request: &mut Request) {
    request.connection = Connection::default();
    request.sql.clear();
}

fn install_panic_redaction() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if thread::current().name() == Some("gridcraft-odbc") {
                let _ = std::io::Write::write_all(&mut std::io::stderr(), b"gridcraft: ODBC driver operation failed\n");
            } else {
                previous(info);
            }
        }));
    });
}
