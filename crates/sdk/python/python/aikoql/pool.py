"""The §3.4 connection pool.

A client-side abstraction over McpClient (no protocol surface — api-v1.json
is frozen). The factory is the seam: each call must return one fully
established session (connected + initialized) — which is also the auth
reset on every reconnect. release() rolls back any open transaction
(session reset: the next borrower never inherits it, §21), reaps
lifetime-expired connections, and hands the connection to a waiter;
acquire() waits up to acquire_timeout and raises the retryable
RESOURCE_EXHAUSTED when the pool stays exhausted. A failed health ping
drops the connection and dials a fresh one; a dead server makes the dial
path retry until the deadline (the reconnect leg of §21).
"""

import threading
import time

from aikoql.mcp_client import McpError


class PooledConnection:
    """One checked-out connection. Client calls delegate to the McpClient;
    begin() pins the transaction so release() can reset the session."""

    def __init__(self, pool: "Pool", client, created_at: float):
        self._pool = pool
        self._client = client
        self._tx = None
        self._created_at = created_at
        self._last_used = created_at
        self._broken = False

    @property
    def client(self):
        """The underlying McpClient (transport-level access)."""
        return self._client

    def __getattr__(self, name):
        # Every McpClient call goes through the checked-out socket;
        # begin() is defined above to pin the transaction.
        return getattr(self._client, name)

    def begin(self, txn_id=None):
        """Open a transaction pinned to this connection (§3.5)."""
        tx = self._client.begin(txn_id)
        self._tx = tx
        return tx

    def release(self):
        """Return the connection to the pool, rolling back a still-open
        transaction first (session reset)."""
        if self._tx is not None and not self._tx._done:
            try:
                self._tx.rollback()
            except Exception:
                self._broken = True  # session reset failed — drop the conn
        self._tx = None
        self._pool._release(self)

    def invalidate(self):
        """Drop the connection (for a caller that saw a transport error)."""
        self._broken = True
        self._pool._release(self)


class Pool:
    """A bounded pool of client connections.

    factory() must return one fully established session (connected +
    initialized) per call. health_check_interval=0.0 pings every borrow.
    """

    def __init__(self, factory, max_connections=10, min_idle=0,
                 acquire_timeout=30.0, idle_timeout=60.0, max_lifetime=3600.0,
                 health_check_interval=30.0):
        self._factory = factory
        self.max_connections = max_connections
        self.min_idle = min_idle
        self.acquire_timeout = acquire_timeout
        self.idle_timeout = idle_timeout
        self.max_lifetime = max_lifetime
        self.health_check_interval = health_check_interval
        # ponytail: one condition + an O(n) idle stack; shard only if a
        # profile ever shows contention.
        self._cond = threading.Condition()
        self._idle = []  # LIFO of PooledConnection, under _cond
        self._total = 0
        self._waiters = 0
        self._closed = False

    def acquire(self):
        """Borrow a connection: an idle one (health-checked when due), a
        fresh dial, or — when the pool is exhausted — it waits up to
        acquire_timeout and raises the retryable RESOURCE_EXHAUSTED."""
        deadline = time.monotonic() + self.acquire_timeout
        while True:
            with self._cond:
                if self._closed:
                    raise McpError(
                        code="UNAVAILABLE",
                        message="connection pool is closed",
                        retryable=True,
                        suggestion="Create a new pool.",
                    )
                while self._idle:
                    pc = self._idle.pop()
                    if self._expired(pc):
                        self._total -= 1
                        pc.client.close()  # under the lock; fine at this scale
                        continue
                    break
                else:
                    pc = None
                if pc is None and self._total < self.max_connections:
                    self._total += 1
                    pc = "dial"
                elif pc is None:
                    self._waiters += 1
                    try:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            raise McpError(
                                code="RESOURCE_EXHAUSTED",
                                message="connection pool exhausted: all "
                                        "connections busy and the acquire "
                                        "timeout elapsed",
                                retryable=True,
                                suggestion="Retry with backoff, or raise "
                                           "max_connections.",
                            )
                        self._cond.wait(timeout=remaining)
                    finally:
                        self._waiters -= 1
                    continue
            if pc == "dial":
                return PooledConnection(self, self._dial_retry(deadline),
                                        time.monotonic())
            # Reused connection: ping when due; a dead one is dropped and
            # replaced by a fresh dial (the reconnect leg of §21).
            if self._health_due(pc) and not self._ping(pc):
                with self._cond:
                    self._total -= 1
                pc.client.close()
                continue
            pc._last_used = time.monotonic()
            return pc

    def _expired(self, pc):
        now = time.monotonic()
        return (self.idle_timeout > 0
                and now - pc._last_used > self.idle_timeout) or \
               (self.max_lifetime > 0
                and now - pc._created_at > self.max_lifetime)

    def _health_due(self, pc):
        if self.health_check_interval == 0.0:
            return True  # ping every borrow
        return time.monotonic() - pc._last_used > self.health_check_interval

    def _ping(self, pc):
        try:
            pc.client.health()
            return True
        except Exception:
            return False

    def _dial_retry(self, deadline):
        """Dial with a short backoff until the deadline — the reconnect
        leg: a dead server keeps the borrow pending and succeeds once the
        server returns."""
        last = None
        while True:
            try:
                return self._factory()
            except Exception as exc:
                last = exc
                if time.monotonic() >= deadline:
                    raise McpError(
                        code="UNAVAILABLE",
                        message=f"could not connect to the server: {last}",
                        retryable=True,
                        suggestion="Verify the server; the pool retries "
                                   "until the acquire timeout.",
                    )
                time.sleep(min(0.1, max(0.0, deadline - time.monotonic())))

    def _release(self, pc):
        """Return a connection: to a waiter when one is blocked, else to
        the idle stack; broken or lifetime-expired connections are dropped."""
        with self._cond:
            drop = (self._closed or pc._broken
                    or (self.max_lifetime > 0
                        and time.monotonic() - pc._created_at
                        > self.max_lifetime))
            if drop:
                self._total -= 1
            else:
                pc._last_used = time.monotonic()
                self._idle.append(pc)
                if self._waiters:
                    self._cond.notify()
        if drop:
            pc.client.close()

    def fill_min_idle(self):
        """Best-effort: dial until min_idle connections sit idle (no
        background refill goroutine)."""
        while True:
            with self._cond:
                if self._closed or len(self._idle) >= self.min_idle or \
                        self._total >= self.max_connections:
                    return
                self._total += 1
            try:
                pc = PooledConnection(
                    self,
                    self._dial_retry(time.monotonic() + self.acquire_timeout),
                    time.monotonic(),
                )
            except Exception:
                with self._cond:
                    self._total -= 1
                raise
            self._release(pc)

    def stats(self):
        with self._cond:
            return {"total": self._total, "idle": len(self._idle)}

    def close(self):
        """Close the idle connections and mark the pool closed; checked-out
        connections are closed when they are released."""
        with self._cond:
            if self._closed:
                return
            self._closed = True
            idle, self._idle = self._idle, []
            self._total -= len(idle)
        for pc in idle:
            pc.client.close()
