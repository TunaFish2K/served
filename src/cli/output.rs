//! Versioned CLI data transfer objects, independent of the manager wire format.
use crate::protocol::{HistoryRecord, ServiceInfo};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub(super) struct Document {
    pub schema_version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
}

#[derive(Serialize)]
pub(super) struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Document {
    pub fn success(data: Value) -> Self {
        Self {
            schema_version: 1,
            ok: true,
            data: Some(data),
            error: None,
        }
    }
    pub fn failure(code: &'static str, message: String) -> Self {
        Self {
            schema_version: 1,
            ok: false,
            data: None,
            error: Some(Failure { code, message }),
        }
    }
}

#[derive(Serialize)]
pub(super) struct Service<'a> {
    name: &'a str,
    directory: &'a str,
    config_file: &'a Option<String>,
    kind: &'static str,
    state: &'static str,
    pid: Option<u32>,
    tty: bool,
    restart: &'a str,
    persist_logs: bool,
    attach_active: bool,
    output_tail: &'a str,
}
impl<'a> From<&'a ServiceInfo> for Service<'a> {
    fn from(s: &'a ServiceInfo) -> Self {
        Self {
            name: &s.name,
            directory: &s.directory,
            config_file: &s.config_file,
            kind: super::format_kind(&s.kind),
            state: super::format_state(&s.state),
            pid: s.pid,
            tty: s.tty,
            restart: &s.restart,
            persist_logs: s.persist_logs,
            attach_active: s.attach_active,
            output_tail: &s.output_tail,
        }
    }
}

#[derive(Serialize)]
pub(super) struct Record<'a> {
    id: &'a str,
    bytes: u64,
    current: bool,
    persisted: bool,
}
impl<'a> From<&'a HistoryRecord> for Record<'a> {
    fn from(r: &'a HistoryRecord) -> Self {
        Self {
            id: &r.id,
            bytes: r.bytes,
            current: r.current,
            persisted: r.persisted,
        }
    }
}
