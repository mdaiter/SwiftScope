// Example usage:
// curl -s -X POST http://127.0.0.1:4000/command -d '{"action":"stacktrace"}' -H 'Content-Type: application/json'
// curl -s -X POST http://127.0.0.1:4000/command -d '{"action":"set_breakpoint","file":"ViewController.swift","line":42}' -H 'Content-Type: application/json'

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{bail, Context};
use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use futures_core::Stream;
use swiftscope::{
    backend,
    debug_session::{self, DebugSession, DebugSessionError},
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    net::{TcpListener, TcpStream},
    process::Command as TokioCommand,
    sync::{broadcast, mpsc, Mutex},
    task::JoinHandle,
    time::sleep,
};

#[derive(Debug, Parser)]
#[command(about = "Start the iOS LLM Debug API server")]
struct Args {
    /// Port where debugserver is listening.
    #[arg(long)]
    debugserver_port: u16,
    /// Optional Mach-O path to use for symbolication (falls back to IOS_LLDB_DAP_CONFIG env).
    #[arg(long)]
    program: Option<PathBuf>,
    /// Host interface for the HTTP server.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// HTTP port for the LLM API.
    #[arg(long, default_value_t = 4000)]
    port: u16,
    /// Optional device identifier for restart/log commands.
    #[arg(long)]
    device: Option<String>,
    /// Optional bundle identifier of the app being debugged.
    #[arg(long)]
    bundle_id: Option<String>,
    /// Require DWARF info to be present.
    #[arg(long)]
    require_dwarf: bool,
    /// Automatically manage ios-llm-devicectl lifecycle.
    #[arg(long)]
    manage_bridge: bool,
    /// Path to the ios-llm-devicectl binary.
    #[arg(long, default_value = "ios-llm-devicectl")]
    ios_llm_devicectl: String,
    /// Additional arguments forwarded to ios-llm-devicectl (repeat flag).
    #[arg(long = "ios-llm-devicectl-arg")]
    ios_llm_devicectl_arg: Vec<String>,
    /// Session state file to reuse between processes.
    #[arg(long)]
    state_file: Option<PathBuf>,
    /// Optional .app bundle to install when launching.
    #[arg(long)]
    app_bundle: Option<PathBuf>,
    /// Enable devicectl log streaming.
    #[arg(long)]
    enable_log_stream: bool,
    /// Path to xcrun (or devicectl wrapper) for log streaming.
    #[arg(long, default_value = "xcrun")]
    devicectl_bin: String,
    /// Subcommand passed to the devicectl wrapper.
    #[arg(long, default_value = "devicectl")]
    devicectl_subcommand: String,
    /// Build command to run when the LLM requests a rebuild (repeat flag).
    #[arg(long = "build-cmd")]
    build_cmd: Vec<String>,
}

#[derive(Clone, Debug)]
struct Config {
    host: String,
    port: u16,
    debugserver_port: u16,
    program: PathBuf,
    device: Option<String>,
    bundle_id: Option<String>,
    require_dwarf: bool,
    manage_bridge: bool,
    ios_llm_devicectl: String,
    ios_llm_devicectl_args: Vec<String>,
    state_file: Option<PathBuf>,
    app_bundle: Option<PathBuf>,
    enable_log_stream: bool,
    devicectl_bin: String,
    devicectl_subcommand: String,
    build_command: Option<Vec<String>>,
}

impl Config {
    fn from_args(args: &Args, program: PathBuf) -> Self {
        Self {
            host: args.host.clone(),
            port: args.port,
            debugserver_port: args.debugserver_port,
            program,
            device: args.device.clone(),
            bundle_id: args.bundle_id.clone(),
            require_dwarf: args.require_dwarf,
            manage_bridge: args.manage_bridge,
            ios_llm_devicectl: args.ios_llm_devicectl.clone(),
            ios_llm_devicectl_args: args.ios_llm_devicectl_arg.clone(),
            state_file: args.state_file.clone(),
            app_bundle: args.app_bundle.clone(),
            enable_log_stream: args.enable_log_stream,
            devicectl_bin: args.devicectl_bin.clone(),
            devicectl_subcommand: args.devicectl_subcommand.clone(),
            build_command: if args.build_cmd.is_empty() {
                None
            } else {
                Some(args.build_cmd.clone())
            },
        }
    }

    fn program(&self) -> &Path {
        &self.program
    }
}

#[derive(Clone)]
struct AppState {
    session: Arc<Mutex<DebugSession>>,
    config: Config,
    bridge: Option<Arc<Mutex<BridgeController>>>,
    log_tx: broadcast::Sender<String>,
    build_runner: Option<Arc<BuildRunner>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action")]
enum LlmCommand {
    #[serde(rename = "stacktrace")]
    Stacktrace,
    #[serde(rename = "threads")]
    Threads,
    #[serde(rename = "continue")]
    Continue,
    #[serde(rename = "next")]
    Next,
    #[serde(rename = "step_in")]
    StepIn,
    #[serde(rename = "set_breakpoint")]
    SetBreakpoint { file: String, line: u32 },
    #[serde(rename = "locals")]
    Locals,
    #[serde(rename = "scopes")]
    Scopes,
    #[serde(rename = "variables")]
    Variables {
        #[serde(rename = "variablesReference", default)]
        reference: Option<i64>,
    },
    #[serde(rename = "evaluate")]
    Evaluate { expression: String },
    #[serde(rename = "evaluate_swift")]
    EvaluateSwift { expression: String },
    #[serde(rename = "watch_expr")]
    WatchExpression { expression: String },
    #[serde(rename = "disconnect")]
    Disconnect,
    #[serde(rename = "restart")]
    Restart,
    #[serde(rename = "launch")]
    Launch,
    #[serde(rename = "build")]
    Build,
    #[serde(rename = "select_thread")]
    SelectThread { thread_id: i64 },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = env_logger::builder().format_timestamp(None).try_init();
    let args = Args::parse();
    let listen_host = args.host.clone();
    let listen_port = args.port;
    let backend = if let Some(program) = args.program.as_deref() {
        debug_session::backend_from_program(program)?
    } else {
        debug_session::init_backend()?
    };

    let mut session = DebugSession::new(backend);
    let config = Config::from_args(&args, session.program_path().to_path_buf());
    let (log_tx, _log_rx) = broadcast::channel(1024);
    let bridge = if config.manage_bridge {
        Some(Arc::new(Mutex::new(BridgeController::new(
            &config,
            log_tx.clone(),
        )?)))
    } else {
        None
    };

    if let Some(controller) = &bridge {
        controller.lock().await.ensure_running().await?;
    }

    if config.enable_log_stream {
        if let Err(err) = spawn_log_stream(&config, log_tx.clone()).await {
            log::warn!("log streaming failed to start: {err}");
        }
    }

    session.connect_debugserver(config.debugserver_port)?;
    warn_if_missing_dwarf(&config)?;

    let build_runner = config
        .build_command
        .as_ref()
        .map(|cmd| Arc::new(BuildRunner::new(cmd.clone())));

    let state = AppState {
        session: Arc::new(Mutex::new(session)),
        config: config.clone(),
        bridge,
        log_tx: log_tx.clone(),
        build_runner,
    };
    let app = Router::new()
        .route("/command", post(handle_command))
        .route("/health", get(health_check))
        .route("/logs", get(stream_logs))
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", listen_host, listen_port).parse()?;
    let listener = TcpListener::bind(addr).await?;
    println!(
        "LLM Debug API listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

async fn handle_command(
    State(state): State<AppState>,
    Json(command): Json<LlmCommand>,
) -> (StatusCode, Json<serde_json::Value>) {
    let response = match command {
        LlmCommand::Restart => handle_launch_request(&state, true)
            .await
            .map_err(|err| DebugSessionError::Backend(err.to_string())),
        LlmCommand::Launch => handle_launch_request(&state, false)
            .await
            .map_err(|err| DebugSessionError::Backend(err.to_string())),
        LlmCommand::Build => handle_build_request(&state)
            .await
            .map_err(|err| DebugSessionError::Backend(err.to_string())),
        other => {
            let mut session = state.session.lock().await;
            execute_command(&mut session, other)
        }
    };

    match response {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": err.to_string() })),
        ),
    }
}

async fn health_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "program": state.config.program.display().to_string(),
        "debugserverPort": state.config.debugserver_port,
        "device": state.config.device,
        "bundleId": state.config.bundle_id,
        "host": state.config.host.clone(),
        "port": state.config.port
    }))
}

fn warn_if_missing_dwarf(config: &Config) -> anyhow::Result<()> {
    if backend::binary_has_dwarf_line_info(config.program()) {
        return Ok(());
    }
    log::warn!(
        "DWARF line info missing for {}. Breakpoints may fail; rebuild the app with a Debug configuration or pass -g.",
        config.program().display()
    );
    if config.require_dwarf {
        bail!(
            "DWARF data missing for {}; refusing to start because --require-dwarf was set",
            config.program().display()
        );
    }
    Ok(())
}

async fn handle_launch_request(state: &AppState, restart: bool) -> anyhow::Result<Value> {
    let bridge = state
        .bridge
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("restart/launch requires --manage-bridge"))?;
    let mut controller = bridge.lock().await;
    if restart {
        controller.restart().await?;
    } else {
        controller.ensure_running().await?;
    }
    drop(controller);
    let mut session = state.session.lock().await;
    session
        .connect_debugserver(state.config.debugserver_port)
        .map_err(|err: DebugSessionError| anyhow::anyhow!(err))?;
    Ok(json!({ "ok": true }))
}

async fn handle_build_request(state: &AppState) -> anyhow::Result<Value> {
    let runner = state
        .build_runner
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("build command not configured"))?;
    let output = runner.run().await?;
    Ok(json!({
        "ok": output.success,
        "exitCode": output.exit_code,
        "stdout": output.stdout,
        "stderr": output.stderr
    }))
}

async fn stream_logs(State(state): State<AppState>) -> impl IntoResponse {
    let mut receiver = state.log_tx.subscribe();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(line) = receiver.recv().await {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Sse::new(LogSseStream { receiver: rx })
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(5)))
}

async fn spawn_log_stream(
    config: &Config,
    log_tx: broadcast::Sender<String>,
) -> anyhow::Result<()> {
    let device = config
        .device
        .clone()
        .ok_or_else(|| anyhow::anyhow!("--device required for log streaming"))?;
    let bin = config.devicectl_bin.clone();
    let subcommand = config.devicectl_subcommand.clone();
    tokio::spawn(async move {
        if let Err(err) = run_log_stream(bin, subcommand, device, log_tx.clone()).await {
            let _ = log_tx.send(format!("log stream exited: {err}"));
        }
    });
    Ok(())
}

async fn run_log_stream(
    bin: String,
    subcommand: String,
    device: String,
    log_tx: broadcast::Sender<String>,
) -> anyhow::Result<()> {
    let mut cmd = TokioCommand::new(&bin);
    if !subcommand.is_empty() {
        cmd.arg(&subcommand);
    }
    cmd.args(["device", "log", "stream", "--device", &device]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {bin} {subcommand} log stream"))?;
    if let Some(stdout) = child.stdout.take() {
        spawn_log_task(stdout, log_tx.clone(), "log");
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_task(stderr, log_tx.clone(), "log-err");
    }
    let status = child.wait().await?;
    let _ = log_tx.send(format!("log stream status: {status}"));
    Ok(())
}

fn spawn_log_task<R>(
    reader: R,
    log_tx: broadcast::Sender<String>,
    tag: &'static str,
) -> JoinHandle<()>
where
    R: AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = log_tx.send(format!("[{tag}] {line}"));
        }
    })
}

struct BridgeController {
    bin: String,
    args: Vec<String>,
    port: u16,
    log_tx: broadcast::Sender<String>,
    handle: Option<BridgeChild>,
}

struct BridgeChild {
    child: tokio::process::Child,
    tasks: Vec<JoinHandle<()>>,
}

impl BridgeController {
    fn new(config: &Config, log_tx: broadcast::Sender<String>) -> anyhow::Result<Self> {
        let device = config
            .device
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--device is required when managing the bridge"))?;
        let bundle = config
            .bundle_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--bundle-id is required when managing the bridge"))?;
        let mut args = config.ios_llm_devicectl_args.clone();
        args.push("--device".into());
        args.push(device);
        args.push("--bundle-id".into());
        args.push(bundle);
        args.push("--listen-port".into());
        args.push(config.debugserver_port.to_string());
        if let Some(app_bundle) = &config.app_bundle {
            args.push("--install-app".into());
            args.push(app_bundle.display().to_string());
        }
        if let Some(state) = &config.state_file {
            args.push("--state-file".into());
            args.push(state.display().to_string());
        }
        Ok(Self {
            bin: config.ios_llm_devicectl.clone(),
            args,
            port: config.debugserver_port,
            log_tx,
            handle: None,
        })
    }

    async fn ensure_running(&mut self) -> anyhow::Result<()> {
        if self.handle.is_none() {
            self.spawn_child().await?;
        }
        Ok(())
    }

    async fn restart(&mut self) -> anyhow::Result<()> {
        self.stop_current().await?;
        self.spawn_child().await
    }

    async fn spawn_child(&mut self) -> anyhow::Result<()> {
        let mut cmd = TokioCommand::new(&self.bin);
        cmd.args(&self.args);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .context("failed to spawn ios-llm-devicectl bridge")?;
        let mut tasks = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            tasks.push(spawn_log_task(stdout, self.log_tx.clone(), "bridge"));
        }
        if let Some(stderr) = child.stderr.take() {
            tasks.push(spawn_log_task(stderr, self.log_tx.clone(), "bridge-err"));
        }
        wait_for_port(self.port).await?;
        self.handle = Some(BridgeChild { child, tasks });
        Ok(())
    }

    async fn stop_current(&mut self) -> anyhow::Result<()> {
        if let Some(mut handle) = self.handle.take() {
            if handle.child.id().is_some() {
                let _ = handle.child.start_kill();
                let _ = handle.child.wait().await;
            }
            for task in handle.tasks {
                task.abort();
            }
        }
        Ok(())
    }
}

async fn wait_for_port(port: u16) -> anyhow::Result<()> {
    let mut attempts = 0;
    loop {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(stream) => {
                drop(stream);
                return Ok(());
            }
            Err(err) => {
                attempts += 1;
                if attempts > 50 {
                    return Err(anyhow::anyhow!(
                        "timed out waiting for bridge on port {port}: {err}"
                    ));
                }
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

struct BuildRunner {
    command: Vec<String>,
}

impl BuildRunner {
    fn new(command: Vec<String>) -> Self {
        Self { command }
    }

    async fn run(&self) -> anyhow::Result<BuildResult> {
        let mut parts = self.command.iter();
        let program = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("build command requires at least one argument"))?;
        let mut cmd = TokioCommand::new(program);
        for arg in parts {
            cmd.arg(arg);
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let output = cmd
            .spawn()
            .context("failed to spawn build command")?
            .wait_with_output()
            .await
            .context("failed to run build command")?;
        Ok(BuildResult {
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or_default(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

struct BuildResult {
    success: bool,
    exit_code: i32,
    stdout: String,
    stderr: String,
}

struct LogSseStream {
    receiver: mpsc::UnboundedReceiver<String>,
}

impl Stream for LogSseStream {
    type Item = Result<Event, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.receiver).poll_recv(cx) {
            Poll::Ready(Some(line)) => Poll::Ready(Some(Ok(Event::default().data(line)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn execute_command(
    session: &mut DebugSession,
    command: LlmCommand,
) -> Result<Value, DebugSessionError> {
    match command {
        LlmCommand::Stacktrace => Ok(json!({ "ok": true, "stacktrace": session.stacktrace() })),
        LlmCommand::Threads => Ok(json!({ "ok": true, "threads": session.threads() })),
        LlmCommand::Continue => session.continue_execution().map(|stop| {
            if let Some(stop) = stop {
                json!({ "ok": true, "stop": stop })
            } else {
                json!({ "ok": true })
            }
        }),
        LlmCommand::Next => session.next().map(|stop| match stop {
            Some(stop) => json!({ "ok": true, "stop": stop }),
            None => json!({ "ok": true }),
        }),
        LlmCommand::StepIn => session.step_in().map(|stop| match stop {
            Some(stop) => json!({ "ok": true, "stop": stop }),
            None => json!({ "ok": true }),
        }),
        LlmCommand::SetBreakpoint { file, line } => session
            .set_breakpoint(&file, line)
            .map(|bp| json!({ "ok": true, "breakpoint_id": bp.id })),
        LlmCommand::Locals => Ok(json!({ "ok": true, "locals": session.locals() })),
        LlmCommand::Scopes => Ok(json!({ "ok": true, "scopes": session.scopes() })),
        LlmCommand::Variables { reference } => {
            let reference = reference.unwrap_or(1);
            Ok(json!({ "ok": true, "variables": session.variables_for_reference(reference) }))
        }
        LlmCommand::Evaluate { expression } => session
            .evaluate(&expression)
            .map(|result| json!({ "ok": true, "result": result.result, "type": result.ty })),
        LlmCommand::EvaluateSwift { expression } => session
            .evaluate_swift(&expression)
            .map(|result| json!({ "ok": true, "result": result.result, "type": result.ty })),
        LlmCommand::WatchExpression { expression } => session
            .add_watch_expression(&expression)
            .map(|values| json!({ "ok": true, "watch": values })),
        LlmCommand::SelectThread { thread_id } => {
            session.select_thread(thread_id);
            Ok(json!({ "ok": true, "threadId": thread_id }))
        }
        LlmCommand::Disconnect => {
            session.disconnect()?;
            Ok(json!({ "ok": true }))
        }
        LlmCommand::Restart | LlmCommand::Launch | LlmCommand::Build => {
            unreachable!("managed by handle_command")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> DebugSession {
        let exe = std::env::current_exe().expect("current exe");
        let backend = debug_session::backend_from_program(&exe).expect("backend");
        DebugSession::new(backend)
    }

    #[test]
    fn stacktrace_returns_frames() {
        let mut session = sample_session();
        let value = execute_command(&mut session, LlmCommand::Stacktrace).unwrap();
        assert!(value.get("stacktrace").is_some());
    }

    #[test]
    fn variables_defaults_to_locals_reference() {
        let mut session = sample_session();
        let value =
            execute_command(&mut session, LlmCommand::Variables { reference: None }).unwrap();
        assert!(value.get("variables").is_some());
    }

    #[test]
    fn evaluate_rejects_empty_input() {
        let mut session = sample_session();
        let err = execute_command(
            &mut session,
            LlmCommand::Evaluate {
                expression: "".into(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("not supported"));
    }

    #[test]
    fn threads_command_returns_payload() {
        let mut session = sample_session();
        let value = execute_command(&mut session, LlmCommand::Threads).unwrap();
        assert!(value.get("threads").is_some());
    }

    #[test]
    fn locals_command_returns_payload() {
        let mut session = sample_session();
        let value = execute_command(&mut session, LlmCommand::Locals).unwrap();
        assert!(value.get("locals").is_some());
    }

    #[test]
    fn scopes_command_returns_payload() {
        let mut session = sample_session();
        let value = execute_command(&mut session, LlmCommand::Scopes).unwrap();
        assert!(value.get("scopes").is_some());
    }

    #[test]
    fn disconnect_command_succeeds_even_without_remote() {
        let mut session = sample_session();
        let value = execute_command(&mut session, LlmCommand::Disconnect).unwrap();
        assert_eq!(value.get("ok").and_then(Value::as_bool), Some(true));
    }

    #[test]
    fn continue_command_errors_without_debugserver() {
        let mut session = sample_session();
        assert!(execute_command(&mut session, LlmCommand::Continue).is_err());
    }

    #[test]
    fn watch_expression_command_returns_entries() {
        let mut session = sample_session();
        let value = execute_command(
            &mut session,
            LlmCommand::WatchExpression {
                expression: "var".into(),
            },
        )
        .unwrap();
        assert!(value.get("watch").is_some());
    }

    #[test]
    fn select_thread_returns_thread_id() {
        let mut session = sample_session();
        let value =
            execute_command(&mut session, LlmCommand::SelectThread { thread_id: 3 }).unwrap();
        assert_eq!(value.get("threadId").and_then(Value::as_i64), Some(3));
    }

    #[cfg(target_family = "unix")]
    #[tokio::test]
    async fn build_runner_executes_command() {
        let runner = BuildRunner::new(vec!["/usr/bin/env".into(), "printf".into(), "ok".into()]);
        let result = runner.run().await.unwrap();
        assert!(result.success);
        assert_eq!(result.stdout, "ok");
    }
}
