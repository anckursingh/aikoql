"""The §3.6 prepared statement: prepare/bind/execute/close.

The initial implementation compiles the AikoQL query client-side on every
execute — the abstraction precedes a native prepare protocol, so the
statement holds no server-side plan (nothing to invalidate, nothing lost
across a server restart). bind() validates the binding and substitutes the
JSON-encoded literals into the query.
"""

import json
import re
import threading

from aikoql.mcp_client import McpError

_PLACEHOLDER = re.compile(r":([a-zA-Z_][a-zA-Z0-9_]*)")


class BoundStatement:
    """A prepared statement with one parameter binding; re-executable."""

    def __init__(self, ps: "PreparedStatement", params: dict):
        self._ps = ps
        self._params = params

    def execute(self) -> dict:
        # ponytail: one lock serializes executes on the statement — the
        # client itself is single-threaded (concurrent calls interleave
        # frames on one socket); a concurrent client would need
        # client-level serialization.
        with self._ps._lock:
            self._ps._guard()
            return self._ps._client.aikoql(
                _substitute(self._ps._query, self._ps._params, self._params))


class PreparedStatement:
    """A compiled-query handle: bind() / execute() / close()."""

    def __init__(self, client, query: str, params: tuple):
        self._client = client
        self._query = query
        self._params = params
        self._closed = False
        self._lock = threading.Lock()  # see BoundStatement.execute

    def bind(self, params: dict) -> BoundStatement:
        """Bind the placeholders; the statement stays reusable."""
        return BoundStatement(self, params or {})

    def execute(self, params: dict = None) -> dict:
        """Bind-and-execute in one call."""
        return self.bind(params).execute()

    def close(self) -> None:
        """Close the statement; further executes are refused. There is no
        server-side handle yet, so this only marks the local state."""
        self._closed = True

    def _guard(self) -> None:
        if self._closed:
            raise McpError(
                code="INVALID_ARGUMENT",
                message="prepared statement is closed",
                suggestion="Prepare the statement again.",
            )


def _substitute(query: str, params: tuple, bound: dict) -> str:
    """Validate the binding (exact placeholder set, scalar values only) and
    inline the JSON-encoded literals. Placeholders are replaced
    longest-first so a name that prefixes another cannot be clobbered.
    ponytail: a bound value containing a ":name"-looking substring is
    replaced blindly — the native protocol removes this class."""
    if len(bound) != len(params):
        raise McpError(
            code="INVALID_ARGUMENT",
            message=f"bound {len(bound)} parameters, the statement has "
                    f"{len(params)} ({', '.join(params)})",
            suggestion="Bind exactly the statement's placeholders.",
        )
    for name in sorted(params, key=len, reverse=True):
        if name not in bound:
            raise McpError(
                code="INVALID_ARGUMENT",
                message=f"parameter :{name} is not bound",
                suggestion="Bind exactly the statement's placeholders.",
            )
        value = bound[name]
        if not _is_scalar(value):
            raise McpError(
                code="INVALID_ARGUMENT",
                message=f"parameter :{name} must be a scalar, got "
                        f"{type(value).__name__}",
                suggestion="Bind scalars (string, number, bool, null) only.",
            )
        query = query.replace(":" + name, json.dumps(value))
    return query


def _is_scalar(value) -> bool:
    return value is None or isinstance(value, (str, int, float, bool))
