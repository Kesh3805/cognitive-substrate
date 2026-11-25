"""Errors raised by the client."""

from __future__ import annotations

from typing import Any


class ApiError(Exception):
    """A non-2xx response from the service.

    The service always answers errors as
    ``{"error": {"code": "...", "message": "..."}}``. ``code`` is that stable,
    machine-readable code (``not_found``, ``validation_failed``,
    ``conflict``, ``unauthorized``, ``invalid_request``, ...). When the body
    does not have that shape (for example a proxy error page), ``code`` is
    ``"unexpected_response"`` and ``message`` holds the raw body text.

    Transport failures (connection refused, timeouts) are not wrapped: they
    surface as the underlying ``httpx`` exceptions.
    """

    status: int
    code: str
    message: str
    body: Any

    def __init__(self, status: int, code: str, message: str, body: Any = None) -> None:
        super().__init__(f"{status} {code}: {message}")
        self.status = status
        self.code = code
        self.message = message
        self.body = body

    def __repr__(self) -> str:
        return f"ApiError(status={self.status!r}, code={self.code!r}, message={self.message!r})"
