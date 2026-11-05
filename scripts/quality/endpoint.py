"""Deterministic fake responder, loopback stub server and loopback-only chat client.

The stub speaks the provisional `chat` wire format of packages/provider-local. Every
reply is a pure function of the canonical request body, so identical requests give
identical replies. Nothing here contacts anything beyond a literal loopback address.
"""

import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, HTTPServer
import ipaddress
import json
import socket
import threading

CHAT_PATH = "/v1/chat/completions"
MAX_BODY_BYTES = 4 * 1024 * 1024
TIMEOUT_SECONDS = 5


class EndpointFailure(Exception):
    """A call failed with one neutral category: `connect`, `timeout`, `http` or `protocol`."""

    def __init__(self, category, status=None):
        super().__init__(category if status is None else f"{category} {status}")
        self.category = category
        self.status = status


def canonical(value):
    """Compact JSON with sorted keys; the byte form every digest is taken over."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def fake_reply(body):
    """Answers a chat request body from its digest alone, without any model."""
    digest = hashlib.sha256(canonical(body).encode()).hexdigest()
    words = sum(len(str(message.get("content") or "").split()) for message in body["messages"])
    tools = body.get("tools") or []
    if tools:
        name = tools[0]["function"]["name"]
        call = {"id": f"call-{digest[:12]}", "type": "function",
                "function": {"name": name, "arguments": canonical({"token": digest[12:28]})}}
        message = {"role": "assistant", "content": None, "tool_calls": [call]}
        output_words = 1
    else:
        text = f"fixture reply {digest[:16]}"
        message = {"role": "assistant", "content": text}
        output_words = len(text.split())
    return {"choices": [{"message": message}],
            "usage": {"prompt_tokens": words, "completion_tokens": output_words}}


class _Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        if self.path != CHAT_PATH or not 0 < length <= MAX_BODY_BYTES:
            self._send(404, {"error": "not_found"})
            return
        try:
            body = json.loads(self.rfile.read(length))
        except ValueError:
            self._send(400, {"error": "invalid_request"})
            return
        if self.server.failure_status is not None:
            self._send(self.server.failure_status, {"error": "unavailable"})
            return
        self._send(200, fake_reply(body))

    def _send(self, status, value):
        data = canonical(value).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_args):
        """Keeps request lines, which carry the ephemeral port, out of the output."""


class StubServer:
    """The fake responder behind an HTTP server on 127.0.0.1 with an ephemeral port.

    `failure_status` makes every chat call answer with that HTTP status instead.
    """

    def __init__(self, failure_status=None):
        self._server = HTTPServer(("127.0.0.1", 0), _Handler)
        self._server.failure_status = failure_status
        self._thread = threading.Thread(target=self._server.serve_forever, args=(0.05,), daemon=True)

    @property
    def port(self):
        return self._server.server_address[1]

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *_exc):
        self._server.shutdown()
        self._server.server_close()
        self._thread.join()


class LoopbackClient:
    """Posts chat requests to a literal loopback address; any other host is refused."""

    def __init__(self, host, port):
        try:
            loopback = ipaddress.ip_address(host).is_loopback
        except ValueError:
            loopback = False
        if not loopback:
            raise ValueError("campaign endpoints must use a literal loopback address")
        self.host = host
        self.port = port

    def complete(self, body):
        data = canonical(body).encode()
        # http.client ignores proxy variables and never follows redirects.
        connection = http.client.HTTPConnection(self.host, self.port, timeout=TIMEOUT_SECONDS)
        try:
            connection.request("POST", CHAT_PATH, body=data,
                               headers={"content-type": "application/json"})
            response = connection.getresponse()
            payload = response.read(MAX_BODY_BYTES + 1)
        except socket.timeout as error:
            raise EndpointFailure("timeout") from error
        except (OSError, http.client.HTTPException) as error:
            raise EndpointFailure("connect") from error
        finally:
            connection.close()
        if response.status != 200:
            raise EndpointFailure("http", response.status)
        if len(payload) > MAX_BODY_BYTES:
            raise EndpointFailure("protocol")
        try:
            return json.loads(payload)
        except ValueError as error:
            raise EndpointFailure("protocol") from error


def parse_reply(reply):
    """Extracts text, tool calls and token counts; raises `protocol` when malformed."""
    try:
        message = reply["choices"][0]["message"]
        text = message.get("content") or ""
        calls = [{"id": call["id"], "name": call["function"]["name"],
                  "input": json.loads(call["function"]["arguments"])}
                 for call in message.get("tool_calls") or []]
    except (KeyError, IndexError, TypeError, AttributeError, ValueError) as error:
        raise EndpointFailure("protocol") from error
    if not isinstance(text, str) or not (text or calls):
        raise EndpointFailure("protocol")
    usage = reply.get("usage") if isinstance(reply.get("usage"), dict) else {}

    def count(name):
        value = usage.get(name)
        return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None

    return {"text": text, "tool_calls": calls,
            "input_tokens": count("prompt_tokens"), "output_tokens": count("completion_tokens")}
