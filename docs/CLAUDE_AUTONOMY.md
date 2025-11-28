# CLAUDE_AUTONOMY.md – Device Deployment & Session Orchestration

**[USER CONFIGURATION REQUIRED]**

Drop this repository into your Claude workspace and reference this guide whenever
you want the agent to take a prompt like “build and debug MyApp on device X”
through a fully automated loop. The structure mirrors the `claude_agents/`
playbook so it plugs into larger multi-agent setups.

---

## 🎯 Mission Profile

**Goal**: Provision an iOS device or simulator, build the target app with DWARF
symbols, start the debugserver bridge, launch the HTTP shim, and keep the system
stable while Claude drives `/command` calls.

**Outcomes**:
- Device paired, trusted, and mounted with a developer disk image.
- Debug binary installed with DWARF.
- `ios-llm-devicectl` running and exposing a localhost gdb-remote port.
- `ios_llm_api --manage-bridge` serving `/command`, `/health`, and `/logs`.
- Claude tool `ios_debug_command` ready for stacktrace/breakpoint/continue loops.

---

## 🧰 Required Tools & Commands

```
Pairing & DDI:
  xcrun devicectl list devices
  xcrun devicectl manage pair --device <udid>
  xcrun devicectl manage ddis install --device <udid>

Build (Debug/DWARF):
  xcodebuild -scheme <scheme> -configuration Debug -destination 'id=<udid>'
  (or swift build -c debug -Xswiftc -g)

Bridge:
  cargo run --features cli --bin ios-llm-devicectl -- \
    --device <udid> --bundle-id <bundle> --install-app <path/to/.app> \
    --listen-port <port> [--state-file .zed/ios-llm-state.json]

HTTP Shim:
  cargo run --features cli --bin ios_llm_api -- \
    --manage-bridge --device <udid> --bundle-id <bundle> \
    --state-file .zed/ios-llm-state.json \
    --debugserver-port <port> --program <path/to/Mach-O> \
    --port 4000 --enable-log-stream \
    [--build-cmd <command> ...]

Automation Shortcut:
  DEVICE=<udid> BUNDLE_ID=<bundle> APP_BUNDLE=/path/MyApp.app \
  make autonomy
```

---

## 🧭 Workflow Overview

1. **Prepare Device** – Pair, trust, and mount DDI. Abort if any step fails.
2. **Build Debug Binary** – Always target Debug configuration so DWARF exists.
3. **Launch Bridge** – `ios-llm-devicectl` installs (optional) and exposes
   debugserver on `127.0.0.1:<port>`, writing `.zed/ios-llm-state.json`.
4. **Start Shim** – `ios_llm_api --manage-bridge` reconnects to the bridge,
   serves `/command`, `/health`, `/logs`, and registers the build hook.
5. **Verify Readiness** – Poll `/health` until `ok:true` with expected program,
   device, and port.
6. **Hand Off to Claude** – Use `ios_debug_command` for stacktrace → set_breakpoint
   → continue, plus advanced actions (`watch_expr`, `select_thread`, `restart`,
   `launch`, `build`).
7. **Monitor** – Stream `/logs`, watch for DWARF warnings, restart if needed.

---

## 🧪 Runnable Example

```
# 1. Pair & mount (one-time unless device changes)
xcrun devicectl list devices
xcrun devicectl manage pair --device ABC123
xcrun devicectl manage ddis install --device ABC123

# 2. Build Debug app
xcodebuild -scheme MyApp -configuration Debug -destination 'id=ABC123'

# 3. Autonomy flow
DEVICE=ABC123 \
BUNDLE_ID=com.example.MyApp \
APP_BUNDLE=/Users/msd/MyApp.app \
make autonomy

# 4. Debug loop (separate shell or Claude tool)
python tools/claude_tool_stub.py --action stacktrace
python tools/claude_tool_stub.py --action set_breakpoint --file ViewController.swift --line 42
python tools/claude_tool_stub.py --action continue

# 5. Logs & health
curl -sf http://127.0.0.1:4000/health
curl -Ns http://127.0.0.1:4000/logs
```

---

## ⚠️ Safeguards & Recovery

| Symptom | Likely Cause | Fix |
|---------|--------------|-----|
| `/health` fails | Shim not running or wrong port | Restart `ios_llm_api`, ensure `--manage-bridge` on |
| `No DWARF ranges...` | App built Release or stripped | Rebuild Debug, rerun bridge |
| Bridge timeout waiting for port | Device trust prompt unseen or port in use | Unlock device, accept prompt, retry |
| `restart` command error | Shim not managing bridge | Relaunch shim with `--manage-bridge` or use manual commands |
| Build command missing | `--build-cmd` not provided | Pass `--build-cmd <script>` when starting shim |

---

## 📓 Configuration Checklist

- [ ] `DEVICE`, `BUNDLE_ID`, `APP_BUNDLE` exported (or provided to CLI).
- [ ] `.zed/ios-llm-state.json` tracked (ignored in git) for reuse.
- [ ] Debug binary path passed via `--program`.
- [ ] `ios_debug_command` schema up to date (see `docs/CLAUDE_TOOL.md`).
- [ ] Logs monitored during runtime (`/logs` SSE or local console).
- [ ] Build hook configured if Claude will trigger rebuilds.

When integrating into a larger Claude environment, include this file in the
agent’s CLAUDE.md references so the orchestrator always follows the approved
workflow.
