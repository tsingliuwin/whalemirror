#!/usr/bin/env python3
"""假 LSP 服务端：只实现 rustdsh `dsh-lsp::client` 联调需要的那点协议。

- `initialize` 回 capabilities，并**主动**发一条 `workspace/configuration` 服务端→客户端
  请求（id 900），用来验证客户端应答方向也能走通；
- `test/config-echo` 回传客户端对那条请求的应答（因此它是双向链路的实证）；
- `test/garbage` 写一段分帧被破坏的字节（非法 JSON），用于验证「解码失败 = 致命关闭」；
- `test/crash` 往 stderr 写一行再以退出码 3 自杀，用于验证退出文案带上 stderr 尾部；
- `test/hang` 不回响应，用于验证子进程死掉时在途请求会被拒绝。

`Content-Length: N\\r\\n\\r\\n` 分帧由本文件自己实现，刻意不复用被测代码。
"""

import json
import os
import sys

LOCATION = {
    "uri": "file:///w/a.ts",
    "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 5}},
}

CAPABILITIES = {
    "positionEncoding": "utf-16",
    "textDocumentSync": 1,
    "definitionProvider": True,
    "referencesProvider": True,
    "implementationProvider": True,
    "hoverProvider": True,
}

CONFIG_REQUEST_ID = 900

config_value = None
pending_echo = []


def read_message():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        stripped = line.strip()
        if stripped == b"":
            break
        if stripped.lower().startswith(b"content-length:"):
            try:
                length = int(stripped.split(b":", 1)[1].strip())
            except ValueError:
                return None
    if length is None:
        return None
    body = sys.stdin.buffer.read(length)
    if len(body) < length:
        return None
    return json.loads(body.decode("utf-8"))


def send(message):
    body = json.dumps(message).encode("utf-8")
    sys.stdout.buffer.write(
        b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body
    )
    sys.stdout.buffer.flush()


def send_raw(data):
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()


def answer_config_request():
    send(
        {
            "jsonrpc": "2.0",
            "id": CONFIG_REQUEST_ID,
            "method": "workspace/configuration",
            "params": {"items": [{"section": "fake"}]},
        }
    )


def response(msg_id, result):
    send({"jsonrpc": "2.0", "id": msg_id, "result": result})


def main():
    global config_value
    while True:
        message = read_message()
        if message is None:
            return
        method = message.get("method")
        msg_id = message.get("id")
        if method is None:
            # 客户端对服务端请求的应答：记下值，并把等着它的 echo 一并结掉。
            if msg_id == CONFIG_REQUEST_ID:
                config_value = message.get("result")
                for pending in pending_echo:
                    response(pending, config_value)
                pending_echo.clear()
            continue
        if method == "initialize":
            response(msg_id, {"capabilities": CAPABILITIES})
            answer_config_request()
            continue
        if method == "initialized":
            continue
        if method == "textDocument/definition":
            response(msg_id, [LOCATION])
            continue
        if method == "textDocument/hover":
            response(
                msg_id,
                {
                    "contents": {"kind": "markdown", "value": "**hover body**"},
                    "range": LOCATION["range"],
                },
            )
            continue
        if method == "test/config-echo":
            if config_value is None:
                pending_echo.append(msg_id)
            else:
                response(msg_id, config_value)
            continue
        if method == "test/garbage":
            # 长度 1 但正文是 `{`：合法分帧、非法 JSON。
            send_raw(b"Content-Length: 1\r\n\r\n{")
            continue
        if method == "test/crash":
            sys.stderr.write("boom: fake server exploded\n")
            sys.stderr.flush()
            os._exit(3)
        if method == "test/hang":
            continue
        if method == "shutdown":
            response(msg_id, None)
            continue
        if method == "exit":
            return
        send(
            {
                "jsonrpc": "2.0",
                "id": msg_id,
                "error": {"code": -32601, "message": "unknown method " + method},
            }
        )


main()