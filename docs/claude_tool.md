# Claude Tool Contract & Stub

This repository already exposes debugger controls over HTTP via `ios-llm-api`
(`cargo run --features cli --bin ios-llm-api -- --debugserver-port <port> --program <Mach-O>`).
The easiest way to let Claude drive those controls is to register a single tool
that mirrors the JSON envelope defined in `src/bin/ios_llm_api.rs`.

## Tool Schema

```json
{
  "name": "ios_debug_command",
  "description": "Send a debugger command to the ios-LLDB HTTP bridge on localhost",
  "input_schema": {
    "type": "object",
    "required": ["action"],
    "properties": {
      "action": {
        "type": "string",
        "description": "Debugger operation to perform",
        "enum": [
          "stacktrace",
          "threads",
          "continue",
          "next",
          "step_in",
          "set_breakpoint",
          "locals",
          "scopes",
          "variables",
          "evaluate",
          "evaluate_swift",
          "watch_expr",
          "select_thread",
          "restart",
          "launch",
          "build",
          "disconnect"
        ]
      },
      "file": {
        "type": "string",
        "description": "Source path for set_breakpoint",
        "nullable": true
      },
      "line": {
        "type": "integer",
        "description": "Line number for set_breakpoint",
        "nullable": true
      },
      "expression": {
        "type": "string",
        "description": "Expression to evaluate",
        "nullable": true
      },
      "threadId": {
        "type": "integer",
        "description": "Thread identifier for select_thread",
        "nullable": true
      },
      "variablesReference": {
        "type": "integer",
        "description": "DAP variablesReference for the variables action",
        "nullable": true
      }
    }
  }
}
```

Claude only needs to set the fields required by the chosen `action`. The HTTP
bridge always responds with:

```json
{ "ok": true, ...payload... }
```

or

```json
{ "ok": false, "error": "<explanation>" }
```

## Python Stub For Teammates

Save `tools/claude_tool_stub.py` somewhere on your PATH and register the
`ios_debug_command` tool in the Claude SDK pointing at `ios_debug_command`.

```python
from typing import Optional

import requests

CLAUDE_TOOL_NAME = "ios_debug_command"


def ios_debug_command(
    *,
    action: str,
    file: Optional[str] = None,
    line: Optional[int] = None,
    expression: Optional[str] = None,
    variables_reference: Optional[int] = None,
    host: str = "127.0.0.1",
    port: int = 4000,
    timeout: float = 5.0,
) -> dict:
    """
    Invoke the ios-LLDB HTTP bridge.

    Claude will emit tool calls that can be forwarded directly to this helper.
    """

    payload: dict = {"action": action}
    if action == "set_breakpoint":
        if not file or line is None:
            raise ValueError("set_breakpoint requires file + line")
        payload["file"], payload["line"] = file, line
    if action == "evaluate":
        if not expression:
            raise ValueError("evaluate requires expression")
        payload["expression"] = expression
    if action == "variables" and variables_reference is not None:
        payload["variablesReference"] = variables_reference

    url = f"http://{host}:{port}/command"
    response = requests.post(url, json=payload, timeout=timeout)
    response.raise_for_status()
    return response.json()
```

### Manual Smoke Test

1. Launch `ios_llm_api` pointing at your adapter and debugserver port.
2. `python tools/claude_tool_stub.py` (or call `run_command` from an interpreter)
   to verify commands like `stacktrace`, `set_breakpoint`, or `locals`.
3. Wire the stub into Claude’s tool registry and map tool invocations directly
   to `ios_debug_command`.

### Command notes

* `evaluate_swift` mirrors `evaluate` today but is reserved for richer Swift
  expressions.
* `watch_expr` stores the provided expression and returns each watch value in
  subsequent calls.
* `select_thread` changes the thread that stack traces, locals, and watches use.
* `restart` / `launch` require `ios_llm_api` to run with `--manage-bridge` so it
  can respawn `ios-llm-devicectl`.
* `build` runs the build hook configured via `--build-cmd`.
