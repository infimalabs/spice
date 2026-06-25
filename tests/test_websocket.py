"""WebSocket upgrade guards: cross-site hijacking is refused on the handshake."""

from __future__ import annotations

import io
from email.message import Message
from http import HTTPStatus
from types import SimpleNamespace

from spice.serve.websocket import WebSocketConnection, accept_websocket


class _FakeHandler:
    def __init__(self, headers: dict[str, str], *, bind=("127.0.0.1", 8765)) -> None:
        message = Message()
        for name, value in headers.items():
            message[name] = value
        self.headers = message
        self.server = SimpleNamespace(server_address=bind)
        self.wfile = io.BytesIO()
        self.errors: list[tuple[int, str | None]] = []
        self.responses: list[int] = []

    def send_error(self, status: int, message: str | None = None) -> None:
        self.errors.append((status, message))

    def send_response(self, status: int) -> None:
        self.responses.append(status)

    def send_header(self, *args, **kwargs) -> None:
        pass

    def end_headers(self) -> None:
        pass


def test_foreign_origin_upgrade_is_refused():
    handler = _FakeHandler(
        {
            "Host": "127.0.0.1:8765",
            "Origin": "https://evil.example",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        }
    )

    connection = accept_websocket(handler)

    assert connection is None
    assert handler.errors == [(HTTPStatus.FORBIDDEN, "cross-origin WebSocket rejected")]
    assert handler.responses == []


def test_same_origin_upgrade_is_accepted():
    handler = _FakeHandler(
        {
            "Host": "127.0.0.1:8765",
            "Origin": "http://127.0.0.1:8765",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        }
    )

    connection = accept_websocket(handler)

    assert isinstance(connection, WebSocketConnection)
    assert handler.responses == [HTTPStatus.SWITCHING_PROTOCOLS]
    assert handler.errors == []


def test_portless_default_http_origin_uses_port_80():
    handler = _FakeHandler(
        {
            "Host": "127.0.0.1:80",
            "Origin": "http://127.0.0.1",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        },
        bind=("127.0.0.1", 80),
    )

    connection = accept_websocket(handler)

    assert isinstance(connection, WebSocketConnection)
    assert handler.responses == [HTTPStatus.SWITCHING_PROTOCOLS]
    assert handler.errors == []


def test_portless_default_https_origin_uses_port_443():
    handler = _FakeHandler(
        {
            "Host": "127.0.0.1:443",
            "Origin": "https://127.0.0.1",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        },
        bind=("127.0.0.1", 443),
    )

    connection = accept_websocket(handler)

    assert isinstance(connection, WebSocketConnection)
    assert handler.responses == [HTTPStatus.SWITCHING_PROTOCOLS]
    assert handler.errors == []


def test_portless_default_http_origin_still_rejects_cross_site():
    handler = _FakeHandler(
        {
            "Host": "127.0.0.1:80",
            "Origin": "http://evil.example",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        },
        bind=("127.0.0.1", 80),
    )

    connection = accept_websocket(handler)

    assert connection is None
    assert handler.errors == [(HTTPStatus.FORBIDDEN, "cross-origin WebSocket rejected")]
    assert handler.responses == []


def test_loopback_host_alias_upgrade_is_accepted():
    handler = _FakeHandler(
        {
            "Host": "localhost:8765",
            "Origin": "http://localhost:8765",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        }
    )

    connection = accept_websocket(handler)

    assert isinstance(connection, WebSocketConnection)
    assert handler.responses == [HTTPStatus.SWITCHING_PROTOCOLS]
    assert handler.errors == []


def test_origin_matching_arbitrary_host_is_refused_on_loopback_bind():
    handler = _FakeHandler(
        {
            "Host": "evil.example:8765",
            "Origin": "http://evil.example:8765",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        }
    )

    connection = accept_websocket(handler)

    assert connection is None
    assert handler.errors == [(HTTPStatus.FORBIDDEN, "cross-origin WebSocket rejected")]
    assert handler.responses == []


def test_missing_origin_upgrade_is_refused():
    handler = _FakeHandler(
        {
            "Host": "127.0.0.1:8765",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        }
    )

    connection = accept_websocket(handler)

    assert connection is None
    assert handler.errors == [(HTTPStatus.FORBIDDEN, "cross-origin WebSocket rejected")]
    assert handler.responses == []


def test_origin_matching_server_bind_is_accepted_when_host_header_absent():
    handler = _FakeHandler(
        {
            "Origin": "http://127.0.0.1:8765",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        }
    )

    connection = accept_websocket(handler)

    assert isinstance(connection, WebSocketConnection)
    assert handler.errors == []


def test_missing_key_still_rejected_after_origin_passes():
    handler = _FakeHandler(
        {
            "Host": "127.0.0.1:8765",
            "Origin": "http://127.0.0.1:8765",
        }
    )

    connection = accept_websocket(handler)

    assert connection is None
    assert handler.errors == [(HTTPStatus.BAD_REQUEST, "missing WebSocket key")]
