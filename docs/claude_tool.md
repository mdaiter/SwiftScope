# CLAUDE_TOOL.md – ios_debug_command Reference

**[USER CONFIGURATION REQUIRED]**

This document mirrors the style of `claude_agents/DEBUGGING.md` and teaches the
agent exactly how to call the lone tool exposed by `ios_llm_api`.

---

## 🧰 Tool Overview

**Purpose**: Bridge Claude and the running debug session via HTTP.

**Endpoints**:
- `POST http://127.0.0.1:<port>/command` – main control surface.
- `GET /health` – readiness check (use before calling commands).
- `GET /logs` – SSE feed (diagnostics).

**Success Envelope**:
```
{ "ok": true, ...payload }
```
**Failure Envelope**:
```
{ "ok": false, "error": "Human-readable explanation" }
```

---

## 🧾 Input Schema

```
{
  "action": "<enum>",
  "file": "<string>",          // set_breakpoint only
  "line": <int>,               // set_breakpoint only
  "expression": "<string>",    // evaluate, evaluate_swift, watch_expr
  "variablesReference": <int>, // variables action
  "threadId": <int>            // select_thread action
}
```

**Supported Actions**

| Category | Actions |
|----------|---------|
| Inspection | `stacktrace`, `threads`, `locals`, `scopes`, `variables` |
| Control | `continue`, `next`, `step_in`, `disconnect` |
| Breakpoints | `set_breakpoint` (requires `file`, `line`) |
| Evaluation | `evaluate`, `evaluate_swift`, `watch_expr` |
| Session Mgmt | `restart`, `launch`, `select_thread`, `build` |

> `restart`/`launch` require `ios_llm_api --manage-bridge`.  
> `build` requires a `--build-cmd` to have been registered on startup.

---

## 📤 Response Contracts

| Action | Payload |
|--------|---------|
| `stacktrace` | `{ "ok": true, "stacktrace": [Frame...] }` |
| `threads` | `{ "ok": true, "threads": [...] }` |
| `set_breakpoint` | `{ "ok": true, "breakpoint_id": <u32> }` |
| `variables` | `{ "ok": true, "variables": [Variable...] }` |
| `evaluate*` | `{ "ok": true, "result": "<value>", "type": "<ty>" }` |
| `watch_expr` | `{ "ok": true, "watch": [{ expression, result }] }` |
| `select_thread` | `{ "ok": true, "threadId": <i64> }` |
| `build` | `{ "ok": <bool>, "exitCode": <int>, "stdout": "...", "stderr": "..." }` |

---

## 🔍 Common Error Patterns

```
ERROR: "expression `<expr>` is not supported"
Cause: Expression not found among locals.
Fix: Inspect locals first or use evaluate_swift.

ERROR: "restart requires --manage-bridge"
Cause: Shim was launched without --manage-bridge.
Fix: Relaunch via `make autonomy` or avoid restart/launch calls.

ERROR: "build command not configured"
Cause: No --build-cmd supplied.
Fix: Start ios_llm_api with `--build-cmd <script>`.
```

---

## 🔎 Diagnostic Procedures

```
Issue: Command hangs
1. curl -sf http://127.0.0.1:4000/health
2. Inspect curl -Ns http://127.0.0.1:4000/logs
3. If bridge died, invoke restart action (requires --manage-bridge)

Issue: Breakpoint skipped
1. Confirm logs lack "No DWARF ranges..."
2. Verify file path matches DWARF (absolute path recommended)
3. Use watch_expr to ensure symbols resolve
```

---

## 🧪 Sample Session

```
python tools/claude_tool_stub.py --action stacktrace
python tools/claude_tool_stub.py --action set_breakpoint --file ViewController.swift --line 42
python tools/claude_tool_stub.py --action continue
python tools/claude_tool_stub.py --action watch_expr --expression counter
python tools/claude_tool_stub.py --action restart     # only if --manage-bridge
```

---

## 📝 Maintenance Notes

1. Keep this schema synced with `src/bin/ios_llm_api.rs`.
2. When adding new actions, document payloads here before exposing to Claude.
3. Reference this file from your top-level `CLAUDE.md` so the orchestrator
   always loads the latest tool contract.
