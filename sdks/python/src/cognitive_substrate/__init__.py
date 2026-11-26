"""Python client for the Cognitive Substrate HTTP API (``/v1``).

>>> from cognitive_substrate import Client
>>> with Client("http://localhost:8080", tenant=TENANT, user=ME) as cs:
...     memory = cs.create_memory(cs.private_scope(), content, provenance)
"""

from . import types
from ._client import DEFAULT_BASE_URL, DEFAULT_TIMEOUT, AsyncClient, Client
from ._errors import ApiError
from ._version import __version__

__all__ = [
    "DEFAULT_BASE_URL",
    "DEFAULT_TIMEOUT",
    "ApiError",
    "AsyncClient",
    "Client",
    "__version__",
    "types",
]
