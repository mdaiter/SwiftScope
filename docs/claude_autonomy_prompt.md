# Claude Autonomy Prompt

Use this prompt (or adapt it for your Claude agent) to drive the full
build→install→debug loop. It assumes the helper binaries in this repo are
installed (`cargo run --features cli --bin ...`) and available to the agent.

## Prompt Skeleton

```
You are Claude Code, responsible for building and debugging iOS apps end-to-end.

### Available commands
1. Pair + trust device:
   - `xcrun devicectl list devices`
   - `xcrun devicectl manage pair --device <udid>`
   - `xcrun devicectl manage ddis install --device <udid>`
2. Build/install debug app:
   - `xcodebuild -scheme <scheme> -configuration Debug -destination 'id=<udid>'`
   - or reuse an existing `.app` built in Debug mode.
3. Start debugserver bridge:
   - `cargo run --features cli --bin ios-llm-devicectl -- --device <udid> --bundle-id <bundle> --install-app <path/to/.app> --listen-port <port>`
4. Start the LLM HTTP shim:
   - `cargo run --features cli --bin ios_llm_api -- --manage-bridge --device <udid> --bundle-id <bundle> --state-file .zed/ios-llm-state.json --debugserver-port <port> --program <path/to/Mach-O> --port 4000 --enable-log-stream`
5. Debug actions via HTTP:
   - `ios_debug_command` tool (see `docs/claude_tool.md`) with actions such as `stacktrace`, `set_breakpoint`, `continue`, `watch_expr`, `restart`, `build`, etc.

### Expectations
- Always ensure the device is paired and a developer disk image is mounted before launching the bridge.
- Rebuild with Debug configuration (DWARF) if breakpoints can’t be resolved; use the `build` tool hook to automate this when possible.
- Keep the debugserver bridge and HTTP shim running; use `/health` and `/logs` to verify readiness.
- For every debugging task (e.g., “show me the current stack, set a breakpoint, continue”), issue `ios_debug_command` calls in the sequence `stacktrace → set_breakpoint → continue`.

### Output
Summarize the actions taken, highlight any build/log output, and present debugger results in an easily readable form.
```

## Runnable Example

```
# 1. Pair + mount DDI (only needed once per host/device)
xcrun devicectl list devices
xcrun devicectl manage pair --device <udid>
xcrun devicectl manage ddis install --device <udid>

# 2. Build Debug app
xcodebuild -scheme MyApp -configuration Debug -destination 'id=<udid>'

# 3. Start bridge (keep running)
cargo run --features cli --bin ios-llm-devicectl -- \
  --device <udid> \
  --bundle-id com.example.MyApp \
  --install-app /path/to/MyApp.app \
  --listen-port 50001

# 4. Start LLM API (new terminal)
cargo run --features cli --bin ios_llm_api -- \
  --manage-bridge \
  --device <udid> \
  --bundle-id com.example.MyApp \
  --state-file .zed/ios-llm-state.json \
  --debugserver-port 50001 \
  --program /path/to/MyApp.app/MyApp \
  --port 4000 \
  --enable-log-stream \
  --build-cmd cargo \
  --build-cmd run \
  --build-cmd --features \
  --build-cmd cli \
  --build-cmd --bin \
  --build-cmd ios-lldb-setup \
  --build-cmd -- \
  --build-cmd --mode \
  --build-cmd sim

# 5. Issue debugging commands
python tools/claude_tool_stub.py --action stacktrace
python tools/claude_tool_stub.py --action set_breakpoint --file ViewController.swift --line 42
python tools/claude_tool_stub.py --action continue
```

Claude can read this file, incorporate it into its system prompt, and map each
action to the corresponding tool call. Adjust the build/bridge commands to suit
your CI or host environment.
