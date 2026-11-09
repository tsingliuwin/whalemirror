#!/usr/bin/env python3
"""最小 MCP stdio 服务端（验证 rustdsh 的 MCP 客户端端到端）。

只实现 initialize / tools/list / tools/call，供实机联调使用：
    python .zcode/mcp_echo_server.py
MCP stdio 帧 = 换行分隔的 JSON-RPC 2.0。
"""
import json
import sys

PROTOCOL = "2026-07-28"

TOOLS = [
    {
        "name": "echo",
        "description": "Echo the given text back.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string", "description": "Text to echo."}},
            "required": ["text"],
        },
    },
    {
        "name": "add",
        "description": "Add two integers.",
        "inputSchema": {
            "type": "object",
            "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
            "required": ["a", "b"],
        },
    },
]


def reply(msg_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result}) + "\n")
    sys.stdout.flush()


def reply_error(msg_id, code, message):
    sys.stdout.write(
        json.dumps({"jsonrpc": "2.0", "id": msg_id, "error": {"code": code, "message": message}}) + "\n"
    )
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "id" not in req:
            continue  # 通知
        method = req.get("method")
        if method == "initialize":
            reply(req["id"], {
                "protocolVersion": PROTOCOL,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "echo-fixture", "version": "1.0.0"},
                "instructions": "This fixture server echoes text and adds integers.",
            })
        elif method == "tools/list":
            reply(req["id"], {"tools": TOOLS})
        elif method == "tools/call":
            params = req.get("params") or {}
            name = params.get("name")
            args = params.get("arguments") or {}
            if name == "echo":
                reply(req["id"], {"content": [{"type": "text", "text": str(args.get("text", ""))}]})
            elif name == "add":
                try:
                    total = int(args.get("a", 0)) + int(args.get("b", 0))
                except (TypeError, ValueError):
                    reply(req["id"], {"isError": True, "content": [{"type": "text", "text": "add requires integers"}]})
                    continue
                reply(req["id"], {"content": [{"type": "text", "text": str(total)}]})
            else:
                reply_error(req["id"], -32601, f"unknown tool {name}")
        else:
            reply_error(req["id"], -32601, f"method not found: {method}")


if __name__ == "__main__":
    main()