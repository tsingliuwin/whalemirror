#!/usr/bin/env python3
"""假 LSP 服务端：只实现 rustdsh `dsh-lsp` 实例/连接/provider 联调需要的那点协议。

- `initialize` 回 capabilities，并**主动**发一条 `workspace/configuration` 服务端→客户端
  请求（id 900），用来验证客户端应答方向；若 argv 带 `--no-hover` 则 hoverProvider=false。
- `test/config-echo` 回传客户端对那条请求的应答（双向链路的实证）。
- `test/open-history` 回 {opens, closes, cancels}：didOpen/didClose/$cancelRequest 的事件史。
- `test/init-count` 回本进程经历过的 initialize 次数（池化的实证）。
- `test/garbage` 写一段分帧被破坏的字节（非法 JSON），验证「解码失败 = 致命关闭」。
- `test/crash` 往 stderr 写一行再以退出码 3 自杀，验证退出文案带上 stderr 尾部。
- `test/die-now` 先回一个结果、再把进程退掉，验证 provider 的透明换新重试。
- `test/hang` 不回响应，验证取消宽限与升级拆卸。
- `test/ignore-shutdown` 置位后忽略 shutdown 请求，验证优雅超时 → 强杀升级。
- `client/registerCapability`（id 901）验证生命周期簿记回 null。

`Content-Length: N\\r\\n\\r\\n` 分帧由本文件自己实现，刻意不复用被测代码。
"""

import json
import os
import sys

LOCATION = {
    "uri": "file:///w/a.ts",
    "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 5}},
}

CONFIG_REQUEST_ID = 900
REGISTER_REQUEST_ID = 901

config_value = None
pending_echo = []
init_count = 0
open_docs = {}
open_history = {"opens": [], "closes": [], "cancels": []}
ignore_shutdown = False


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


def response(msg_id, result):
    send({"jsonrpc": "2.0", "id": msg_id, "result": result})


def answer_config_request():
    send(
        {
            "jsonrpc": "2.0",
            "id": CONFIG_REQUEST_ID,
            "method": "workspace/configuration",
            "params": {"items": [{"section": "fake"}]},
        }
    )


def answer_register_request():
    send(
        {
            "jsonrpc": "2.0",
            "id": REGISTER_REQUEST_ID,
            "method": "client/registerCapability",
            "params": {"registrations": []},
        }
    )


def main():
    global config_value, init_count, ignore_shutdown
    no_hover = "--no-hover" in sys.argv[1:]
    hang_hover = "--hang-hover" in sys.argv[1:]
    capabilities = {
        "positionEncoding": "utf-16",
        "textDocumentSync": 1,
        "definitionProvider": True,
        "referencesProvider": True,
        "implementationProvider": True,
        "hoverProvider": False if no_hover else True,
    }

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
            if msg_id == REGISTER_REQUEST_ID:
                # 客户端对 registerCapability 的应答：不需要动作。
                pass
            continue
        if method == "initialize":
            init_count += 1
            response(msg_id, {"capabilities": capabilities})
            answer_config_request()
            answer_register_request()
            continue
        if method == "initialized":
            continue
        if method == "textDocument/didOpen":
            doc = message.get("params", {}).get("textDocument", {})
            open_docs[doc.get("uri")] = doc.get("text")
            open_history["opens"].append(doc.get("uri"))
            continue
        if method == "textDocument/didClose":
            uri = message.get("params", {}).get("textDocument", {}).get("uri")
            open_docs.pop(uri, None)
            open_history["closes"].append(uri)
            continue
        if method == "$/cancelRequest":
            open_history["cancels"].append(message.get("params", {}).get("id"))
            continue
        if method == "textDocument/definition":
            response(msg_id, [LOCATION])
            continue
        if method == "textDocument/hover":
            if hang_hover:
                # 不响应：用来测取消宽限与升级拆卸。
                continue
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
        if method == "test/open-history":
            response(msg_id, open_history)
            continue
        if method == "test/init-count":
            response(msg_id, init_count)
            continue
        if method == "test/garbage":
            send_raw(b"Content-Length: 1\r\n\r\n{")
            continue
        if method == "test/crash":
            sys.stderr.write("boom: fake server exploded\n")
            sys.stderr.flush()
            os._exit(3)
        if method == "test/die-now":
            response(msg_id, "dying")
            sys.stdout.buffer.flush()
            os._exit(0)
        if method == "test/hang":
            continue
        if method == "test/ignore-shutdown":
            ignore_shutdown = True
            response(msg_id, "ignoring shutdown from now on")
            continue
        if method == "shutdown":
            if ignore_shutdown:
                continue
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