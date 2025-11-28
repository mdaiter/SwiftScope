use std::{
    collections::{BTreeSet, HashMap},
    env, io,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::{
    backend::{Backend, BackendStopEvent},
    CONFIG_ENV_VAR,
};

const DEFAULT_THREAD_ID: i64 = 1;
const LOCALS_REFERENCE: i64 = 1;

pub type BreakpointId = u32;

pub struct DebugSession {
    backend: Backend,
    thread_id: i64,
    next_breakpoint_id: BreakpointId,
    file_breakpoints: HashMap<String, BTreeSet<i64>>,
    watch_expressions: Vec<String>,
}

impl DebugSession {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend,
            thread_id: DEFAULT_THREAD_ID,
            next_breakpoint_id: 1,
            file_breakpoints: HashMap::new(),
            watch_expressions: Vec::new(),
        }
    }

    pub fn connect_debugserver(&mut self, port: u16) -> Result<(), DebugSessionError> {
        self.backend
            .connect_debugserver(port)
            .map_err(DebugSessionError::Backend)
    }

    pub fn stacktrace(&self) -> Vec<Frame> {
        self.backend
            .stack_trace(self.thread_id)
            .into_iter()
            .enumerate()
            .map(|(idx, value)| Frame::from_backend_value(idx, &value))
            .collect()
    }

    pub fn threads(&self) -> Vec<Value> {
        self.backend.threads()
    }

    pub fn scopes(&self) -> Vec<Value> {
        self.backend.scopes()
    }

    pub fn continue_execution(&mut self) -> Result<Option<SessionStop>, DebugSessionError> {
        self.backend
            .r#continue(self.thread_id)
            .map(|maybe_event| maybe_event.map(SessionStop::from))
            .map_err(DebugSessionError::Backend)
    }

    pub fn next(&mut self) -> Result<Option<SessionStop>, DebugSessionError> {
        self.backend
            .step_over(self.thread_id)
            .map(|maybe_event| maybe_event.map(SessionStop::from))
            .map_err(DebugSessionError::Backend)
    }

    pub fn step_in(&mut self) -> Result<Option<SessionStop>, DebugSessionError> {
        self.backend
            .step_in(self.thread_id)
            .map(|maybe_event| maybe_event.map(SessionStop::from))
            .map_err(DebugSessionError::Backend)
    }

    pub fn disconnect(&mut self) -> Result<(), DebugSessionError> {
        self.backend
            .disconnect()
            .map_err(DebugSessionError::Backend)
    }

    pub fn set_breakpoint(
        &mut self,
        file: &str,
        line: u32,
    ) -> Result<Breakpoint, DebugSessionError> {
        let entry = self
            .file_breakpoints
            .entry(file.to_string())
            .or_insert_with(BTreeSet::new);
        entry.insert(line as i64);
        let current_lines: Vec<i64> = entry.iter().copied().collect();
        self.backend
            .update_breakpoints(file, &current_lines)
            .map_err(DebugSessionError::Backend)?;

        let id = self.next_breakpoint_id;
        self.next_breakpoint_id = self.next_breakpoint_id.saturating_add(1);
        Ok(Breakpoint {
            id,
            file: file.to_string(),
            line,
        })
    }

    pub fn locals(&self) -> Vec<Variable> {
        self.variables_for_reference(LOCALS_REFERENCE)
    }

    pub fn variables_for_reference(&self, reference: i64) -> Vec<Variable> {
        self.backend
            .variables(reference)
            .into_iter()
            .map(Variable::from_backend_value)
            .collect()
    }

    pub fn evaluate(&self, expression: &str) -> Result<EvalResult, DebugSessionError> {
        let trimmed = expression.trim();
        if trimmed.is_empty() {
            return Err(DebugSessionError::UnsupportedExpression(
                expression.to_string(),
            ));
        }
        let locals = self.locals();
        if let Some(variable) = locals.iter().find(|var| var.name == trimmed) {
            return Ok(EvalResult {
                result: variable.value.clone(),
                ty: variable.ty.clone(),
            });
        }
        Err(DebugSessionError::UnsupportedExpression(
            expression.to_string(),
        ))
    }

    pub fn evaluate_swift(&self, expression: &str) -> Result<EvalResult, DebugSessionError> {
        self.evaluate(expression)
    }

    pub fn add_watch_expression(
        &mut self,
        expression: &str,
    ) -> Result<Vec<WatchValue>, DebugSessionError> {
        let trimmed = expression.trim();
        if trimmed.is_empty() {
            return Err(DebugSessionError::UnsupportedExpression(
                expression.to_string(),
            ));
        }
        if !self
            .watch_expressions
            .iter()
            .any(|existing| existing == trimmed)
        {
            self.watch_expressions.push(trimmed.to_string());
        }
        self.evaluate_watch_expressions()
    }

    pub fn evaluate_watch_expressions(&self) -> Result<Vec<WatchValue>, DebugSessionError> {
        self.watch_expressions
            .iter()
            .map(|expr| {
                self.evaluate(expr).map(|result| WatchValue {
                    expression: expr.clone(),
                    result,
                })
            })
            .collect()
    }

    pub fn select_thread(&mut self, thread_id: i64) {
        self.thread_id = thread_id.max(1);
    }

    pub fn program_path(&self) -> &Path {
        self.backend.program_path()
    }
}

#[derive(Debug, Error)]
pub enum DebugSessionError {
    #[error("{0}")]
    Backend(String),
    #[error("expression `{0}` is not supported")]
    UnsupportedExpression(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Frame {
    pub frame_index: usize,
    pub function: String,
    pub file: String,
    pub line: u32,
}

impl Frame {
    fn from_backend_value(index: usize, value: &Value) -> Self {
        let function = value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string();
        let file = value
            .get("source")
            .and_then(|src| src.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string();
        let line = value
            .get("line")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            .max(0) as u32;
        Self {
            frame_index: index,
            function,
            file,
            line,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Variable {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub value: String,
}

impl Variable {
    fn from_backend_value(value: Value) -> Self {
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unnamed>")
            .to_string();
        let ty = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string();
        let val = value
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Self {
            name,
            ty,
            value: val,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalResult {
    pub result: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchValue {
    pub expression: String,
    pub result: EvalResult,
}

#[derive(Debug, Clone, Serialize)]
pub struct Breakpoint {
    pub id: BreakpointId,
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionStop {
    pub reason: String,
    pub description: String,
    pub thread_id: i64,
}

impl From<BackendStopEvent> for SessionStop {
    fn from(value: BackendStopEvent) -> Self {
        Self {
            reason: value.reason.to_string(),
            description: value.description,
            thread_id: value.thread_id,
        }
    }
}

pub fn init_backend() -> io::Result<Backend> {
    if let Ok(raw) = env::var(CONFIG_ENV_VAR) {
        if let Some(program) = parse_program_from_config(&raw)? {
            return backend_from_program(&program);
        }
    }
    let exe = env::current_exe()?;
    backend_from_program(&exe)
}

pub fn backend_from_program(program: &Path) -> io::Result<Backend> {
    Backend::new_from_app(program).map_err(|err| io::Error::new(io::ErrorKind::Other, err))
}

pub fn parse_program_from_config(raw: &str) -> io::Result<Option<PathBuf>> {
    let value: Value =
        serde_json::from_str(raw).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    Ok(value
        .get("program")
        .and_then(Value::as_str)
        .map(PathBuf::from))
}
