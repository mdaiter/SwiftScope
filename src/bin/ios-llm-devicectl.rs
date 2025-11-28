use std::{
    fs,
    io::{self, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde_json::{json, Value};

#[derive(Debug, Parser)]
#[command(about = "Launch debugserver over devicectl and bridge it to a local port")]
struct Args {
    /// Device identifier understood by devicectl (UDID/name/serial).
    #[arg(long)]
    device: String,
    /// Bundle identifier to start (devicectl --start-stopped).
    #[arg(long)]
    bundle_id: String,
    /// Optional .app path to install before launching.
    #[arg(long)]
    install_app: Option<PathBuf>,
    /// Local TCP port to expose the gdb-remote connection on.
    #[arg(long, default_value_t = 2331)]
    listen_port: u16,
    /// Path to debugserver on the device.
    #[arg(long, default_value = "/Developer/usr/libexec/debugserver")]
    debugserver_path: String,
    /// Path to the devicectl wrapper (usually xcrun).
    #[arg(long, default_value = "xcrun")]
    devicectl: String,
    /// Subcommand to invoke inside the devicectl wrapper.
    #[arg(long, default_value = "devicectl")]
    devicectl_subcommand: String,
    /// Additional arguments forwarded to `devicectl device process launch` (app launch step).
    #[arg(long = "launch-arg")]
    launch_arg: Vec<String>,
    /// Path to a state file that records the last launch metadata.
    #[arg(long)]
    state_file: Option<PathBuf>,
}

#[derive(Debug)]
struct LaunchResult {
    pid: i64,
    app_binary: Option<PathBuf>,
}

fn main() -> Result<()> {
    env_logger::builder().format_timestamp(None).init();
    let args = Args::parse();
    if let Some(app) = &args.install_app {
        install_app(&args, app)?;
    }
    let launch = launch_app_waiting(&args)?;
    println!(
        "Process {} for bundle {} is suspended and awaiting debugserver",
        launch.pid, args.bundle_id
    );
    if let Some(binary) = &launch.app_binary {
        println!("Detected app binary at {}", binary.display());
    } else {
        println!("devicectl output did not reveal an app binary path");
    }
    if let Err(err) = write_state_file(&args, &launch) {
        eprintln!("failed to record session state: {err}");
    }
    let child = spawn_debugserver(&args, launch.pid)?;
    bridge_stdio(child, args.listen_port)?;
    Ok(())
}

fn write_state_file(args: &Args, launch: &LaunchResult) -> Result<()> {
    let path = args
        .state_file
        .clone()
        .unwrap_or_else(|| PathBuf::from(".zed/ios-llm-state.json"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let canonical_app = launch
        .app_binary
        .as_ref()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .or_else(|| launch.app_binary.clone());
    let state = json!({
        "device": args.device,
        "bundle_id": args.bundle_id,
        "listen_port": args.listen_port,
        "app_binary": canonical_app.as_ref().map(|p| p.display().to_string()),
    });
    fs::write(&path, serde_json::to_string_pretty(&state)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    println!("Wrote session metadata to {}", path.display());
    Ok(())
}

fn install_app(args: &Args, app: &Path) -> Result<()> {
    println!("Installing {} to {}", app.display(), args.device);
    let mut cmd = base_command(args);
    cmd.args([
        "device",
        "install",
        "--device",
        &args.device,
        app.to_str()
            .ok_or_else(|| anyhow!("non-unicode app path: {}", app.display()))?,
    ]);
    let status = cmd.status().context("failed to run devicectl install")?;
    if !status.success() {
        return Err(anyhow!("devicectl install failed: {status}"));
    }
    Ok(())
}

fn launch_app_waiting(args: &Args) -> Result<LaunchResult> {
    let json_path = temp_json_path("launch");
    let mut cmd = base_command(args);
    cmd.args([
        "device",
        "process",
        "launch",
        "--device",
        &args.device,
        "--start-stopped",
        "--terminate-existing",
    ]);
    for extra in &args.launch_arg {
        cmd.arg(extra);
    }
    cmd.args([
        "-j",
        json_path
            .to_str()
            .ok_or_else(|| anyhow!("json path is not valid utf-8"))?,
        &args.bundle_id,
    ]);
    let status = cmd.status().context("failed to run devicectl launch")?;
    if !status.success() {
        return Err(anyhow!("devicectl launch failed: {status}"));
    }
    let data = fs::read_to_string(&json_path)
        .with_context(|| format!("failed to read json output {}", json_path.display()))?;
    let value: Value = serde_json::from_str(&data).context("invalid devicectl json")?;
    fs::remove_file(&json_path).ok();
    let pid = extract_process_identifier(&value)
        .ok_or_else(|| anyhow!("launch output missing process identifier: {value:?}"))?;
    let app_binary = extract_app_binary(&value);
    Ok(LaunchResult { pid, app_binary })
}

fn spawn_debugserver(args: &Args, pid: i64) -> Result<Child> {
    println!("Spawning debugserver for pid {pid}");
    let mut cmd = base_command(args);
    cmd.args([
        "device",
        "process",
        "launch",
        "--device",
        &args.device,
        "--console",
        args.debugserver_path.as_str(),
        "stdio",
        &format!("--attach={pid}"),
    ]);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.spawn()
        .context("failed to launch debugserver via devicectl")
}

fn bridge_stdio(mut child: Child, port: u16) -> Result<()> {
    let mut child_stdout = child
        .stdout
        .take()
        .context("debugserver stdout not captured")?;
    let mut child_stdin = child
        .stdin
        .take()
        .context("debugserver stdin not captured")?;
    if let Some(mut stderr) = child.stderr.take() {
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stderr.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let _ = io::stderr().write_all(&buf[..n]);
            }
        });
    }

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("failed to bind port {port}"))?;
    println!("gdb-remote bridge listening on 127.0.0.1:{port}");
    let (mut stream, addr) = listener
        .accept()
        .context("failed to accept adapter connection")?;
    println!("Adapter connected from {addr}");

    let mut stream_for_stdin = stream.try_clone().context("failed to clone tcp stream")?;
    let writer = thread::spawn(move || {
        let _ = io::copy(&mut stream_for_stdin, &mut child_stdin);
    });
    io::copy(&mut child_stdout, &mut stream).context("bridge stdout copy failed")?;
    let _ = writer.join();
    let _ = child.kill();
    let _ = child.wait();
    println!("Debugserver session finished");
    Ok(())
}

fn base_command(args: &Args) -> Command {
    let mut cmd = Command::new(&args.devicectl);
    if !args.devicectl_subcommand.is_empty() {
        cmd.arg(&args.devicectl_subcommand);
    }
    cmd
}

fn temp_json_path(prefix: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    std::env::temp_dir().join(format!("ios_llm_{prefix}_{ts}.json"))
}

fn extract_process_identifier(value: &Value) -> Option<i64> {
    find_i64(value, "processIdentifier").or_else(|| find_i64(value, "pid"))
}

fn find_i64(value: &Value, key: &str) -> Option<i64> {
    match value {
        Value::Object(map) => {
            if let Some(v) = map.get(key) {
                if let Some(as_i64) = v.as_i64() {
                    return Some(as_i64);
                }
            }
            map.values().find_map(|entry| find_i64(entry, key))
        }
        Value::Array(items) => items.iter().find_map(|entry| find_i64(entry, key)),
        _ => None,
    }
}

fn extract_app_binary(value: &Value) -> Option<PathBuf> {
    fn as_pathbuf(value: &Value) -> Option<PathBuf> {
        value.as_str().map(|s| PathBuf::from(s.trim()))
    }
    match value {
        Value::Object(map) => {
            for key in [
                "app_binary",
                "appBinary",
                "executablePath",
                "executableURL",
                "appExecutable",
                "program",
            ] {
                if let Some(path) = map.get(key).and_then(as_pathbuf) {
                    return Some(path);
                }
            }
            map.values().find_map(extract_app_binary)
        }
        Value::Array(items) => items.iter().find_map(extract_app_binary),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_pid_in_nested_json() {
        let value = serde_json::json!({
            "status": "success",
            "result": {
                "processIdentifier": 4242
            }
        });
        assert_eq!(extract_process_identifier(&value), Some(4242));
    }

    #[test]
    fn fallback_to_pid_key() {
        let value = serde_json::json!({ "pid": 1337 });
        assert_eq!(extract_process_identifier(&value), Some(1337));
    }

    #[test]
    fn extracts_app_binary_from_json() {
        let value = serde_json::json!({
            "status": "ok",
            "result": {
                "app_binary": "/tmp/MyApp.app/MyApp"
            }
        });
        let path = extract_app_binary(&value).expect("missing app_binary");
        assert!(path.ends_with("MyApp"), "unexpected path: {:?}", path);
    }
}
