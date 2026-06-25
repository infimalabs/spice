from __future__ import annotations

import base64
import hashlib
import ipaddress
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
ORIGIN_DEFAULT_PORTS = {"http": 80, "https": 443}


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

    The server bind address is always allowed. The Host header is allowed only
    when it is itself compatible with that bind: loopback, the same explicit
    bind host, or any host on an intentional wildcard bind. This keeps a DNS
    rebinding page from making Origin and Host match on an arbitrary domain
    while the socket actually reaches the loopback-bound server.

    On a wildcard bind, that last rule intentionally degrades the Origin guard
    to Origin-equals-Host for any host on the bound port. It is not the
    rebinding-resistant authority match used for loopback or explicit binds;
    the serve auth token is the operative defense on that path.
    """
    authorities: set[str] = set()
    server = getattr(handler, "server", None)
    server_address = getattr(server, "server_address", None)
    bind_host: str | None = None
    bind_port: int | None = None
    if isinstance(server_address, tuple) and len(server_address) >= 2:
        bind_host = str(server_address[0]).strip().lower()
        try:
            bind_port = int(server_address[1])
        except (TypeError, ValueError):
            bind_port = None
    if bind_host and bind_port is not None:
        authorities.add(_format_authority(bind_host, bind_port))
    host_header = (handler.headers.get("Host") or "").strip().lower()
    host_parts = _authority_parts(host_header)
    if host_parts is not None and _host_authority_allowed(
        host_parts[0], host_parts[1], bind_host=bind_host, bind_port=bind_port
    ):
        authorities.add(_format_authority(host_parts[0], host_parts[1]))
    return authorities


def websocket_origin_allowed(handler: Any) -> bool:
    """Reject cross-site WebSocket hijacking on the upgrade.

    Browsers always send ``Origin`` on a WebSocket handshake and cannot forge
    it, so an ``Origin`` whose authority does not match an allowed bind or
    loopback target is a cross-site page driving the live bus. Missing
    ``Origin`` is refused too; the live bus is a browser UI surface, not a
    compatibility API for raw WebSocket clients.
    """
    origin = handler.headers.get("Origin")
    if not origin:
        return False
    parsed_origin = urlsplit(origin)
    origin_parts = _authority_parts(
        parsed_origin.netloc.lower(),
        default_port=ORIGIN_DEFAULT_PORTS.get(parsed_origin.scheme.lower()),
    )
    if origin_parts is None:
        return False
    origin_authority = _format_authority(origin_parts[0], origin_parts[1])
    return origin_authority in websocket_request_authorities(handler)


def _authority_parts(
    authority: str, *, default_port: int | None = None
) -> tuple[str, int] | None:
    if not authority:
        return None
    parsed = urlsplit(f"//{authority}")
    host = (parsed.hostname or "").strip().lower()
    if not host:
        return None
    try:
        port = parsed.port
    except ValueError:
        return None
    if port is None:
        if default_port is None:
            return None
        port = default_port
    return host, port


def _host_authority_allowed(
    host: str,
    port: int,
    *,
    bind_host: str | None,
    bind_port: int | None,
) -> bool:
    if bind_port is None or port != bind_port:
        return False
    if _host_is_loopback(host):
        return True
    if bind_host is None:
        return False
    if _host_is_wildcard_bind(bind_host):
        return True
    return host == bind_host


def _host_is_loopback(host: str) -> bool:
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def _host_is_wildcard_bind(host: str) -> bool:
    try:
        return ipaddress.ip_address(host).is_unspecified
    except ValueError:
        return host == ""


def _format_authority(host: str, port: int) -> str:
    if ":" in host and not host.startswith("["):
        return f"[{host}]:{port}"
    return f"{host}:{port}"


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
