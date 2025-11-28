# Devicectl-Based Autonomous Loop

The combination of `ios-llm-devicectl`, `ios_llm_api`, and the Claude tool stub
forms an end-to-end loop that never relies on `iproxy`. Everything goes through
`devicectl` and debugserver’s `stdio` transport.

## Moving Parts

1. **Deployment** – `ios-llm-devicectl --install-app <.app>` shells out to
   `xcrun devicectl device install --device <udid>`.
2. **Launch + Suspend** – the same binary issues
   `devicectl device process launch --start-stopped` and parses the JSON output
   for `processIdentifier`.
3. **Debugserver bridge** – `devicectl device process launch --console
   /Developer/usr/libexec/debugserver stdio --attach=<pid>` keeps the debugserver
   stdio channel alive; `ios-llm-devicectl` binds it to `127.0.0.1:<listen_port>`
   so the Rust adapter can talk gdb-remote locally.
4. **LLM control surface** – `ios_llm_api --debugserver-port <listen_port>
   --program /path/to/Mach-O` exposes `/command` for Claude (see
   `tools/claude_tool_stub.py`).

## Running ios-llm-devicectl On Hardware

1. Pair the device once (if it is new):

   ```bash
   xcrun devicectl list devices
   xcrun devicectl manage pair --device <udid>
   xcrun devicectl manage ddis install --device <udid>
   ```

2. Build or locate the `.app` you want to debug.
3. Launch the bridge:

   ```bash
   cargo run --features cli --bin ios-llm-devicectl -- \
     --device <udid-or-name> \
     --bundle-id com.example.MyApp \
     --install-app /path/to/MyApp.app \
     --listen-port 50001 \
     --debugserver-path /Developer/usr/libexec/debugserver
   ```

   *Omit `--install-app` when the bundle is already on the device.*

4. In a second terminal, start the HTTP API that Claude will talk to:

   ```bash
   cargo run --features cli --bin ios_llm_api -- \
     --debugserver-port 50001 \
     --program /absolute/path/to/MyApp.app/MyApp \
     --port 4000
   ```

5. Point the Claude tool stub at `http://127.0.0.1:4000/command` and let the
   agent drive `stacktrace`, `set_breakpoint`, `continue`, etc.

Every time `ios-llm-devicectl` launches it records the detected app binary and
port in `.zed/ios-llm-state.json`. Other tooling (like `make autonomy`) can read
that file to pick the correct `APP_PROGRAM` automatically.

### Manual relay helpers

If you prefer to manage the transport yourself (for example, when the device is
reachable over SSH), forward the debugserver port manually:

```bash
# devicectl relay
xcrun devicectl device relay --device <udid> 50001:2331

# or plain SSH
ssh -L 50001:127.0.0.1:2331 user@remote-mac
```

Then point `ios_llm_api --debugserver-port 50001 ...` at the forwarded port.

## Log Streaming

Run the HTTP shim with `--enable-log-stream --device <udid>` to spawn
`devicectl device log stream`. The log feed is exposed as an SSE endpoint:

```bash
curl -N http://127.0.0.1:4000/logs
```

Claude (or any other agent) can consume the same endpoint to watch device logs,
crash reports, or stdout/stderr.

## Suggested Automation Script

```bash
cargo run --features cli --bin ios-llm-devicectl -- \
  --device <udid> \
  --bundle-id com.example.MyApp \
  --install-app /path/to/MyApp.app \
  --listen-port 50001 &
BRIDGE_PID=$!

cargo run --features cli --bin ios-llm-api -- \
  --debugserver-port 50001 \
  --program /absolute/path/to/MyApp.app/MyApp \
  --port 4000 &
API_PID=$!

# Claude now owns the loop via the ios_debug_command tool.
# When finished:
kill $API_PID
kill $BRIDGE_PID
```

Once Claude connects, it can:

* `set_breakpoint` against any source file.
* `continue`, `next`, or `step_in` until the stop payload indicates a hit.
* `locals`, `variables`, and `evaluate` to inspect state.
* `stacktrace`, `threads`, `scopes` for context.
* `disconnect` to tear down the remote debugserver.

If Claude hits an error (`"ok": false`), it can reissue commands or regenerate
code, creating the debug ⇄ edit loop with zero human intervention.

## Tests & Protocol Confidence

* **Dummy coverage** – `src/bin/ios_llm_api.rs` and
  `src/bin/ios-llm-devicectl.rs` include unit tests for command dispatch and PID
  parsing, respectively.
* **Full protocol runthrough** – `tests/dap_harness.rs` spawns the DAP adapter,
  feeds it the same commands Zed/LLM will use, and verifies that stack traces
  are produced. Run `cargo test --features cli dap_harness_produces_stack_trace`
  before letting Claude drive a device.

With the bridge pinned to localhost and the HTTP shim exposing deterministic
responses, your LLM agents can deploy, attach, set breakpoints, inspect
variables, learn from failures, and iterate their code generation entirely via
`devicectl` and the APIs in this repository.

## Make Target

`make autonomy` wraps the script above. Configure the knobs once:

```bash
export DEVICE=<udid>
export BUNDLE_ID=com.example.MyApp
export APP_BUNDLE=/path/to/MyApp.app        # optional
export APP_PROGRAM=/path/to/MyApp.app/MyApp # required
# optional customizations:
# export BRIDGE_PORT=50001
# export API_PORT=4000

make autonomy
```

The target now auto-fills `APP_PROGRAM` from `.zed/ios-llm-state.json` (if
available), starts `ios-llm-devicectl` (install + launch + debugserver bridge),
spawns `ios_llm_api --manage-bridge`, and hits `/health` until the HTTP shim is
ready. Press `Ctrl+C` to terminate both processes together.

## Smoke Test

With `ios_llm_api` running locally (either via `make autonomy` or manual
commands), you can prove the Claude wiring in three requests using the Python
stub:

```bash
python tools/claude_tool_stub.py --action stacktrace
python tools/claude_tool_stub.py --action set_breakpoint --file ViewController.swift --line 42
python tools/claude_tool_stub.py --action continue
```

Repeat the same sequence against a managed device or simulator—the commands are
identical because `ios-llm-devicectl` keeps the debugserver tunnel alive and
the shim reconnects automatically when `restart`/`launch` are invoked.
