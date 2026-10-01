package aikoql

// The §3.4 connection pool: a client-side abstraction over Dial (no
// protocol surface — api-v1.json is frozen). The factory is the seam:
// each call must return one fully established session (dialed +
// initialized), which is also the auth reset on every reconnect. Release
// rolls back any open transaction (session reset — the next borrower
// never inherits it, §21), reaps lifetime-expired connections, and hands
// the connection to a waiter; Acquire waits up to AcquireTimeout and
// returns the retryable RESOURCE_EXHAUSTED when the pool stays exhausted.
// A failed health ping drops the connection and dials a fresh one; a dead
// server makes the dial path retry until the deadline (the reconnect leg
// of §21).

import (
	"context"
	"fmt"
	"sync"
	"time"
)

// PoolConfig configures a Pool. Only Factory is required; zero values
// fall back to the defaults named on each field.
type PoolConfig struct {
	// Factory returns one fully established session per call.
	Factory func(ctx context.Context) (*Client, error)
	// MaxConns bounds the pool (default 10).
	MaxConns int
	// MinIdle is honored by FillMinIdle (default 0; no background refill).
	MinIdle int
	// AcquireTimeout bounds a waiter (default 30s; RESOURCE_EXHAUSTED).
	AcquireTimeout time.Duration
	// IdleTimeout reaps a connection idle longer than this (default 60s).
	IdleTimeout time.Duration
	// MaxLifetime reaps a connection older than this (default 1h).
	MaxLifetime time.Duration
	// HealthCheckInterval pings a borrowed connection when it sat idle
	// longer than this (default 30s; a near-zero value pings every borrow).
	HealthCheckInterval time.Duration
}

// PooledConn is one checked-out connection. Client calls go through
// Client(); Begin pins the transaction so Release can reset the session.
type PooledConn struct {
	p         *Pool
	c         *Client
	tx        *Tx
	createdAt time.Time
	lastUsed  time.Time
	broken    bool
}

// Client returns the underlying client (transport-level access).
func (pc *PooledConn) Client() *Client { return pc.c }

// Begin opens a transaction pinned to this connection; Release rolls it
// back if it is still open.
func (pc *PooledConn) Begin(ctx context.Context, txnID ...string) (*Tx, error) {
	tx, err := pc.c.Begin(ctx, txnID...)
	if err != nil {
		return nil, err
	}
	pc.tx = tx
	return tx, nil
}

// Release returns the connection to the pool, rolling back a still-open
// transaction first (session reset).
func (pc *PooledConn) Release() error {
	if pc.tx != nil && !pc.tx.done {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		if err := pc.tx.Rollback(ctx); err != nil {
			pc.broken = true // session reset failed — drop the connection
		}
	}
	pc.tx = nil
	pc.p.put(pc)
	return nil
}

// Invalidate drops the connection (for a caller that saw a transport
// error and does not trust the connection anymore).
func (pc *PooledConn) Invalidate() {
	pc.broken = true
	pc.p.put(pc)
}

// PoolStats is the observable pool shape.
type PoolStats struct {
	Total int
	Idle  int
}

// Pool is a bounded pool of client connections.
type Pool struct {
	cfg PoolConfig
	mu  sync.Mutex
	// ponytail: one mutex, an O(n) idle stack, and a broadcast channel
	// (a thundering herd on every return) — per-waiter queues only if a
	// profile ever shows contention.
	idle    []*PooledConn
	total   int
	waiters int
	notify  chan struct{}
	closed  bool
}

// NewPool builds a pool with the defaults filled in.
func NewPool(cfg PoolConfig) *Pool {
	if cfg.Factory == nil {
		panic("aikoql: PoolConfig.Factory is required")
	}
	if cfg.MaxConns <= 0 {
		cfg.MaxConns = 10
	}
	if cfg.AcquireTimeout <= 0 {
		cfg.AcquireTimeout = 30 * time.Second
	}
	if cfg.IdleTimeout <= 0 {
		cfg.IdleTimeout = 60 * time.Second
	}
	if cfg.MaxLifetime <= 0 {
		cfg.MaxLifetime = time.Hour
	}
	if cfg.HealthCheckInterval <= 0 {
		cfg.HealthCheckInterval = 30 * time.Second
	}
	return &Pool{cfg: cfg, notify: make(chan struct{})}
}

// Acquire returns a connection: an idle one (health-checked when due), a
// fresh dial, or — when the pool is exhausted — it waits for a return up
// to AcquireTimeout and fails with the retryable RESOURCE_EXHAUSTED.
func (p *Pool) Acquire(ctx context.Context) (*PooledConn, error) {
acquire:
	for {
		p.mu.Lock()
		if p.closed {
			p.mu.Unlock()
			return nil, &McpError{Code: "UNAVAILABLE", Message: "connection pool is closed",
				Retryable: true, Suggestion: "Create a new pool."}
		}
		for len(p.idle) > 0 {
			pc := p.idle[len(p.idle)-1]
			p.idle = p.idle[:len(p.idle)-1]
			if p.expired(pc) {
				p.total--
				p.mu.Unlock()
				_ = pc.c.Close()
				continue acquire
			}
			now := time.Now()
			if now.Sub(pc.lastUsed) > p.cfg.HealthCheckInterval {
				// Ping outside the lock; a dead connection is dropped and
				// replaced by a fresh dial (the reconnect leg of §21).
				p.mu.Unlock()
				if _, err := pc.c.Health(ctx); err != nil {
					p.mu.Lock()
					p.total--
					p.mu.Unlock()
					_ = pc.c.Close()
					continue acquire
				}
				pc.lastUsed = now
				return pc, nil
			}
			pc.lastUsed = now
			p.mu.Unlock()
			return pc, nil
		}
		if p.total < p.cfg.MaxConns {
			p.total++
			p.mu.Unlock()
			c, err := p.dialRetry(ctx)
			if err != nil {
				p.mu.Lock()
				p.total--
				p.mu.Unlock()
				return nil, err
			}
			now := time.Now()
			return &PooledConn{p: p, c: c, createdAt: now, lastUsed: now}, nil
		}
		// Exhausted: wait for a return.
		p.waiters++
		gen := p.notify
		timer := time.NewTimer(p.cfg.AcquireTimeout)
		p.mu.Unlock()
		var err error
		select {
		case <-gen: // a connection came back — re-attempt the acquire
		case <-ctx.Done():
			err = ctx.Err()
		case <-timer.C:
			err = &McpError{Code: "RESOURCE_EXHAUSTED",
				Message:    "connection pool exhausted: all connections busy and the acquire timeout elapsed",
				Retryable:  true,
				Suggestion: "Retry with backoff, or raise MaxConns."}
		}
		timer.Stop()
		p.mu.Lock()
		p.waiters--
		p.mu.Unlock()
		if err != nil {
			return nil, err
		}
	}
}

// dialRetry calls the factory with a short backoff until the deadline —
// the reconnect leg: a dead server keeps the borrow pending and succeeds
// once the server returns.
func (p *Pool) dialRetry(ctx context.Context) (*Client, error) {
	deadline := time.Now().Add(p.cfg.AcquireTimeout)
	if d, ok := ctx.Deadline(); ok && d.Before(deadline) {
		deadline = d
	}
	var lastErr error
	for {
		c, err := p.cfg.Factory(ctx)
		if err == nil {
			return c, nil
		}
		lastErr = err
		if ctx.Err() != nil || time.Now().After(deadline) {
			return nil, &McpError{Code: "UNAVAILABLE",
				Message:    fmt.Sprintf("could not connect to the server: %v", lastErr),
				Retryable:  true,
				Suggestion: "Verify the server; the pool retries until the acquire timeout."}
		}
		select {
		case <-time.After(100 * time.Millisecond):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
}

// expired reports whether an idle connection was reaped by the idle or
// lifetime timeout.
func (p *Pool) expired(pc *PooledConn) bool {
	now := time.Now()
	return now.Sub(pc.lastUsed) > p.cfg.IdleTimeout ||
		now.Sub(pc.createdAt) > p.cfg.MaxLifetime
}

// put returns a connection to the pool: to a waiter when one is blocked,
// else to the idle stack; lifetime-expired or broken connections are
// dropped instead.
func (p *Pool) put(pc *PooledConn) {
	now := time.Now()
	pc.lastUsed = now
	p.mu.Lock()
	drop := p.closed || pc.broken || now.Sub(pc.createdAt) > p.cfg.MaxLifetime
	if !drop {
		p.idle = append(p.idle, pc)
		if p.waiters > 0 {
			close(p.notify)
			p.notify = make(chan struct{})
		}
	} else {
		p.total--
	}
	p.mu.Unlock()
	if drop {
		_ = pc.c.Close()
	}
}

// FillMinIdle dials until MinIdle connections sit idle (best-effort, no
// background refill goroutine).
func (p *Pool) FillMinIdle(ctx context.Context) error {
	for {
		p.mu.Lock()
		if p.closed || len(p.idle) >= p.cfg.MinIdle || p.total >= p.cfg.MaxConns {
			p.mu.Unlock()
			return nil
		}
		p.total++
		p.mu.Unlock()
		c, err := p.dialRetry(ctx)
		if err != nil {
			p.mu.Lock()
			p.total--
			p.mu.Unlock()
			return err
		}
		now := time.Now()
		p.put(&PooledConn{p: p, c: c, createdAt: now, lastUsed: now})
	}
}

// Stats reports the observable pool shape.
func (p *Pool) Stats() PoolStats {
	p.mu.Lock()
	defer p.mu.Unlock()
	return PoolStats{Total: p.total, Idle: len(p.idle)}
}

// Close closes the idle connections and marks the pool closed; checked-
// out connections are closed when they are released.
func (p *Pool) Close() error {
	p.mu.Lock()
	if p.closed {
		p.mu.Unlock()
		return nil
	}
	p.closed = true
	idle := p.idle
	p.idle = nil
	p.total -= len(idle)
	p.mu.Unlock()
	for _, pc := range idle {
		_ = pc.c.Close()
	}
	return nil
}
