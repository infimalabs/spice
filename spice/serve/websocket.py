from __future__ import annotations

import base64
import hashlib
import json
from dataclasses import dataclass, field
from http import HTTPStatus
import struct
from threading import Lock
from typing import Any
from urllib.parse import urlsplit

WEBSOCKET_ACCEPT_SUFFIX = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
MAX_TEXT_FRAME_BYTES = 1024 * 1024
WEBSOCKET_FIN_BIT = 0x80
WEBSOCKET_OPCODE_MASK = 0x0F
WEBSOCKET_CLIENT_MASK_BIT = 0x80
WEBSOCKET_PAYLOAD_LENGTH_MASK = 0x7F
WEBSOCKET_PAYLOAD_16BIT_LENGTH = 126
WEBSOCKET_PAYLOAD_64BIT_LENGTH = 127
WEBSOCKET_PAYLOAD_16BIT_MAX = 0xFFFF
WEBSOCKET_HEADER_BYTES = 2
WEBSOCKET_MASK_BYTES = 4
WEBSOCKET_EXTENDED_16BIT_LENGTH_BYTES = 2
WEBSOCKET_EXTENDED_64BIT_LENGTH_BYTES = 8
WEBSOCKET_TEXT_OPCODE = 0x1
WEBSOCKET_CLOSE_OPCODE = 0x8
WEBSOCKET_PING_OPCODE = 0x9
WEBSOCKET_PONG_OPCODE = 0xA


class WebSocketDisconnect(Exception):
    """Raised when the WebSocket peer closes or disconnects."""


class WebSocketProtocolError(Exception):
    """Raised for unsupported or malformed WebSocket frames."""


@dataclass
class WebSocketConnection:
    handler: Any
    writer_lock: Lock = field(default_factory=Lock)

    def read_json(self) -> dict[str, Any]:
        text = self.read_text()
        try:
            payload = json.loads(text)
        except json.JSONDecodeError as exc:
            raise WebSocketProtocolError("invalid JSON WebSocket message") from exc
        if not isinstance(payload, dict):
            raise WebSocketProtocolError("WebSocket message must be a JSON object")
        return payload

    def read_text(self) -> str:
        while True:
            opcode, payload = self._read_frame()
            if opcode == WEBSOCKET_TEXT_OPCODE:
                try:
                    return payload.decode("utf-8")
                except UnicodeDecodeError as exc:
                    raise WebSocketProtocolError(
                        "invalid UTF-8 WebSocket text"
                    ) from exc
            if opcode == WEBSOCKET_CLOSE_OPCODE:
                self.close()
                raise WebSocketDisconnect()
            if opcode == WEBSOCKET_PING_OPCODE:
                self._write_frame(WEBSOCKET_PONG_OPCODE, payload)
                continue
            if opcode == WEBSOCKET_PONG_OPCODE:
                continue
            raise WebSocketProtocolError(f"unsupported WebSocket opcode {opcode}")

    def send_json(self, payload: dict[str, Any]) -> None:
        text = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self._write_frame(WEBSOCKET_TEXT_OPCODE, text)

    def ping(self, payload: bytes = b"") -> None:
        self._write_frame(WEBSOCKET_PING_OPCODE, payload)

    def set_read_timeout(self, seconds: float | None) -> None:
        # Bound how long a blocking frame read waits. A client that has gone
        # silent past this window — no app-level ping, no data — is treated as
        # disconnected so its server thread and rollout watchers are reaped
        # instead of leaking until the kernel's multi-hour TCP keepalive fires.
        self.handler.connection.settimeout(seconds)

    def close(self) -> None:
        try:
            self._write_frame(WEBSOCKET_CLOSE_OPCODE, b"")
        except OSError:
            return

    def _read_frame(self) -> tuple[int, bytes]:
        header = _read_exact(self.handler.rfile, WEBSOCKET_HEADER_BYTES)
        first, second = header
        if not first & WEBSOCKET_FIN_BIT:
            raise WebSocketProtocolError("fragmented WebSocket frames are unsupported")
        opcode = first & WEBSOCKET_OPCODE_MASK
        masked = bool(second & WEBSOCKET_CLIENT_MASK_BIT)
        if not masked:
            raise WebSocketProtocolError("client WebSocket frames must be masked")
        length = second & WEBSOCKET_PAYLOAD_LENGTH_MASK
        if length == WEBSOCKET_PAYLOAD_16BIT_LENGTH:
            length = struct.unpack(
                "!H",
                _read_exact(self.handler.rfile, WEBSOCKET_EXTENDED_16BIT_LENGTH_BYTES),
            )[0]
        elif length == WEBSOCKET_PAYLOAD_64BIT_LENGTH:
            length = struct.unpack(
                "!Q",
                _read_exact(self.handler.rfile, WEBSOCKET_EXTENDED_64BIT_LENGTH_BYTES),
            )[0]
        if length > MAX_TEXT_FRAME_BYTES:
            raise WebSocketProtocolError("WebSocket frame is too large")
        mask = _read_exact(self.handler.rfile, WEBSOCKET_MASK_BYTES)
        payload = _read_exact(self.handler.rfile, length)
        return opcode, bytes(
            byte ^ mask[index % WEBSOCKET_MASK_BYTES]
            for index, byte in enumerate(payload)
        )

    def _write_frame(self, opcode: int, payload: bytes) -> None:
        if len(payload) > MAX_TEXT_FRAME_BYTES:
            raise WebSocketProtocolError("WebSocket frame is too large")
        first = WEBSOCKET_FIN_BIT | opcode
        length = len(payload)
        if length < WEBSOCKET_PAYLOAD_16BIT_LENGTH:
            header = struct.pack("!BB", first, length)
        elif length <= WEBSOCKET_PAYLOAD_16BIT_MAX:
            header = struct.pack(
                "!BBH",
                first,
                WEBSOCKET_PAYLOAD_16BIT_LENGTH,
                length,
            )
        else:
            header = struct.pack(
                "!BBQ",
                first,
                WEBSOCKET_PAYLOAD_64BIT_LENGTH,
                length,
            )
        with self.writer_lock:
            self.handler.wfile.write(header + payload)
            self.handler.wfile.flush()


def is_websocket_request(handler: Any) -> bool:
    upgrade = (handler.headers.get("Upgrade") or "").lower()
    connection = (handler.headers.get("Connection") or "").lower()
    return upgrade == "websocket" and "upgrade" in connection


def websocket_request_authorities(handler: Any) -> set[str]:
    """The host:port authorities a same-origin WebSocket may carry.

    The Host header is what the browser actually connected to; the server bind
    address is added as belt-and-suspenders. Both are lowercased so the Origin
    comparison is case-insensitive on the host.
    """
    authorities: set[str] = set()
    host_header = (handler.headers.get("Host") or "").strip().lower()
    if host_header:
        authorities.add(host_header)
    server = getattr(handler, "server", None)
    server_address = getattr(server, "server_address", None)
    if isinstance(server_address, tuple) and len(server_address) >= 2:
        authorities.add(f"{server_address[0]}:{server_address[1]}".lower())
    return authorities


def websocket_origin_allowed(handler: Any) -> bool:
    """Reject cross-site WebSocket hijacking on the upgrade.

    A browser always sends ``Origin`` on a WebSocket handshake and cannot
    forge it, so an ``Origin`` whose authority does not match the request
    target is a cross-site page (a malicious tab in the operator's browser)
    driving the live bus — refuse it. A missing ``Origin`` is a non-browser
    client, which is not a confused-deputy vector, so it is allowed.
    """
    origin = handler.headers.get("Origin")
    if not origin:
        return True
    origin_authority = urlsplit(origin).netloc.lower()
    if not origin_authority:
        return False
    return origin_authority in websocket_request_authorities(handler)


def accept_websocket(handler: Any) -> WebSocketConnection | None:
    if not websocket_origin_allowed(handler):
        handler.send_error(HTTPStatus.FORBIDDEN, "cross-origin WebSocket rejected")
        return None
    key = handler.headers.get("Sec-WebSocket-Key") or ""
    if not key:
        handler.send_error(HTTPStatus.BAD_REQUEST, "missing WebSocket key")
        return None
    accept = base64.b64encode(
        hashlib.sha1(f"{key}{WEBSOCKET_ACCEPT_SUFFIX}".encode("ascii")).digest()
    ).decode("ascii")
    handler.send_response(HTTPStatus.SWITCHING_PROTOCOLS)
    handler.send_header("Upgrade", "websocket")
    handler.send_header("Connection", "Upgrade")
    handler.send_header("Sec-WebSocket-Accept", accept)
    handler.end_headers()
    handler.wfile.flush()
    return WebSocketConnection(handler)


def _read_exact(reader: Any, length: int) -> bytes:
    try:
        data = reader.read(length)
    except (TimeoutError, OSError) as exc:
        # A read timeout (silent client past the socket deadline) or a reset
        # peer both mean the connection is gone; surface a clean disconnect so
        # the session tears down instead of crashing the handler thread.
        raise WebSocketDisconnect() from exc
    if len(data) != length:
        raise WebSocketDisconnect()
    return data
