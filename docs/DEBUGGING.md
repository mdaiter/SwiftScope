# DEBUGGING.md – Log Patterns & Troubleshooting

**[USER CONFIGURATION REQUIRED]**

Agents consult this file whenever `/logs` or local stdout shows unexpected
output. Structure mirrors `claude_agents/DEBUGGING.md`.

---

## 📋 Log Format & Patterns

```
Format:
[TIMESTAMP] [LEVEL] [COMPONENT] Message

Examples:
2025-02-10T12:00:05Z [INFO] [Bridge] Process 4321 for bundle com.example.MyApp is suspended
2025-02-10T12:00:07Z [WARN] [ios_llm_api] DWARF line info missing for /path/MyApp
2025-02-10T12:00:09Z [ERROR] [ios_llm_api] failed to spawn ios-llm-devicectl bridge: ...

Levels:
- INFO – lifecycle steps (install, connect, watch updates)
- WARN – recoverable issues (missing DWARF, retries)
- ERROR – action failed (devicectl exit, build failure)

Destinations:
- stdout/stderr of both binaries
- `/logs` SSE stream when `--enable-log-stream` is enabled
- Optional log files ignored via `.gitignore` (bridge.log, ios_llm_api.log)
```

---

## 🔍 Common Error Patterns

```
"DWARF line info missing for <path>"
Cause: App built Release/stripped.
Fix: Rebuild Debug, rerun bridge; use --require-dwarf to fail fast.

"failed to spawn ios-llm-devicectl bridge"
Cause: Missing pairing, invalid bundle id, devicectl not installed.
Fix: Run pairing commands, confirm `--bundle-id`, ensure xcrun available.

"timed out waiting for bridge on port <N>"
Cause: Device trust dialog pending or port already bound.
Fix: Unlock/approve device, pick another port or kill conflicting process.

"restart requires --manage-bridge"
Cause: Shim launched without bridge management.
Fix: Start via `make autonomy` or include `--manage-bridge`.
```

---

## 🔎 Diagnostic Procedures

```
Issue: No response from /command
1. curl -sf http://127.0.0.1:4000/health
2. If healthy, retry command; if not, restart shim.
3. Check /logs for devicectl or build failures.
4. Ensure ios-llm-devicectl process is still running (ps aux | grep ios-llm).

Issue: Breakpoints never hit
1. Confirm logs show "Detected app binary ..." when bridge started.
2. Verify DWARF warning absent.
3. Check file paths (use absolute Swift file paths).
4. Run watch_expr to ensure variables resolve.
```

---

## 🚨 Error Categories & Priority

| Severity | Definition | Response |
|----------|------------|----------|
| Critical (P0) | Bridge/shim down, `/health` fails repeatedly | Immediate restart or rebuild |
| High (P1) | Build failures, DWARF missing, pairing issues | Fix within hour |
| Medium (P2) | Watch expressions failing, log stream gaps | Triage same day |
| Low (P3) | Informational warnings, duplicate breakpoints | Log and move on |

---

## 🔧 Debugging Tools & Techniques

```
- `curl -sf http://127.0.0.1:4000/health`
- `curl -Ns http://127.0.0.1:4000/logs`
- `python tools/claude_tool_stub.py --action <command>`
- `make autonomy` for full restart
- `xcrun devicectl ...` commands for pairing/DDI
```

---

## 🐛 Known Issues & Workarounds

```
Restart fails if shim not managing bridge.
→ Workaround: always launch via make autonomy (sets --manage-bridge).

Build command long and error-prone.
→ Workaround: wrap build logic in shell script, reference via --build-cmd <script>.
```

---

## 🔄 Debugging Workflow

1. Verify `/health`.
2. Stream `/logs` while reproducing.
3. Use `stacktrace` + `set_breakpoint` + `continue`.
4. Add `watch_expr` for key variables.
5. If app exits, call `restart` then reapply breakpoints.
6. Run `build` whenever code changed.
