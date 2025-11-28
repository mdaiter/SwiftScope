#!/usr/bin/env python3
"""
Minimal helper that forwards Claude tool calls to the ios-LLDB HTTP API.
"""

from __future__ import annotations

import argparse
import json
import sys
from typing import Any, Dict, Optional

import requests


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
    thread_id: Optional[int] = None,
) -> Dict[str, Any]:
    """
    Send a debugger command to the local ios-LLDB HTTP surface.
    """

    payload: Dict[str, Any] = {"action": action}
    if action == "set_breakpoint":
        if not file or line is None:
            raise ValueError("set_breakpoint requires --file and --line")
        payload["file"] = file
        payload["line"] = line
    if action in {"evaluate", "evaluate_swift", "watch_expr"}:
        if not expression:
            raise ValueError(f"{action} requires --expression")
        payload["expression"] = expression
    if action == "variables" and variables_reference is not None:
        payload["variablesReference"] = variables_reference
    if action == "select_thread":
        if thread_id is None:
            raise ValueError("select_thread requires --thread-id")
        payload["threadId"] = thread_id

    resp = requests.post(
        f"http://{host}:{port}/command", json=payload, timeout=timeout
    )
    resp.raise_for_status()
    return resp.json()


def main() -> None:
    parser = argparse.ArgumentParser(description="LLM tool stub tester")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=4000)
    parser.add_argument("--action", required=True)
    parser.add_argument("--file")
    parser.add_argument("--line", type=int)
    parser.add_argument("--expression")
    parser.add_argument("--variables-reference", type=int)
    parser.add_argument("--thread-id", type=int)
    args = parser.parse_args()
    response = ios_debug_command(
        action=args.action,
        file=args.file,
        line=args.line,
        expression=args.expression,
        variables_reference=args.variables_reference,
        thread_id=args.thread_id,
        host=args.host,
        port=args.port,
    )
    json.dump(response, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
