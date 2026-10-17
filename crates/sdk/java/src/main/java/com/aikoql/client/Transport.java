package com.aikoql.client;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.Queue;

// Newline-framed TCP transport with one waiting reader: the session
// serializes calls, so at most one readLine is pending at a time; a line
// that arrives while nobody waits is queued for the next read (the §3.3
// late-response self-heal). The abort path (deadline) and cancelPendingRead
// (stream teardown) wake the waiter, exactly like the TS transport's
// signal.resolve.
final class Transport implements AutoCloseable {
    /** §19: refuse a line before it can grow past the cap mid-read
     * (readLine() would allocate the whole line first). The cap counts
     * chars — UTF-8 decodes one char to at most 4 bytes, so the bound
     * holds either way. */
    private static final int MAX_FRAME = 1024 * 1024;

    private final Socket socket;
    private final BufferedReader in;
    private final OutputStream out;
    private final Thread reader;

    private final Queue<String> lines = new ArrayDeque<>();
    private boolean closed;
    private String pending;
    private boolean pendingDone;
    private AikoqlException pendingErr;
    private Deadline registeredDl;

    static Transport connect(String addr, int timeoutMs) {
        int colon = addr.lastIndexOf(':');
        if (colon <= 0) throw AikoqlException.invalidArgument("addr must be host:port, got " + addr);
        int port;
        try {
            port = Integer.parseInt(addr.substring(colon + 1));
        } catch (NumberFormatException e) {
            throw AikoqlException.invalidArgument("addr must be host:port, got " + addr);
        }
        Socket s = new Socket();
        try {
            s.connect(new InetSocketAddress(addr.substring(0, colon), port), timeoutMs);
            return new Transport(s);
        } catch (IOException e) {
            try {
                s.close();
            } catch (IOException ignored) {
            }
            throw AikoqlException.io("connect " + addr + ": " + e.getMessage());
        }
    }

    private Transport(Socket socket) throws IOException {
        this.socket = socket;
        this.in = new BufferedReader(
                new InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8));
        this.out = socket.getOutputStream();
        this.reader = new Thread(this::readLoop, "aikoql-reader");
        reader.setDaemon(true);
        reader.start();
    }

    /** The next line, or null when the connection closed. An aborted
     * deadline (past or present) surfaces as the frozen retryable TIMEOUT. */
    String readLine(Deadline dl) {
        synchronized (this) {
            if (dl != null) {
                dl.onAbort(() -> {
                    synchronized (Transport.this) {
                        if (registeredDl == dl) {
                            registeredDl = null;
                            pendingErr = AikoqlException.deadline();
                            // pendingDone stays false: that flag means "a
                            // line was delivered" — a premature null here
                            // would latch the session closed.
                            Transport.this.notifyAll();
                        }
                    }
                });
                registeredDl = dl;
                if (dl.isAborted()) {
                    // Abort raced registration: its wake-up ran or will
                    // run, but never for this waiter — fail here and clear.
                    pendingErr = null;
                    pendingDone = false;
                    registeredDl = null;
                    throw AikoqlException.deadline();
                }
            }
            for (;;) {
                if (!lines.isEmpty()) return lines.poll();
                if (pendingErr != null) {
                    // Checked before closed: the cap sets both (the stream
                    // is desynced) and the error must win over a bare EOF.
                    AikoqlException e = pendingErr;
                    pendingErr = null;
                    // The abort set the pending marker alongside the error;
                    // clear both or the next read returns a phantom null.
                    pending = null;
                    pendingDone = false;
                    registeredDl = null;
                    throw e;
                }
                if (closed) return null;
                try {
                    wait();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                    throw AikoqlException.io("interrupted waiting for a response");
                }
                if (pendingDone) {
                    String l = pending;
                    pending = null;
                    pendingDone = false;
                    registeredDl = null;
                    return l;
                }
            }
        }
    }

    /** Resolves the pending read with null (EOF) — the stream-teardown
     * path. A no-op when nothing waits. */
    void cancelPendingRead() {
        synchronized (this) {
            if (registeredDl != null) {
                registeredDl = null;
                pending = null;
                pendingDone = true;
                notifyAll();
            }
        }
    }

    void write(String line) {
        synchronized (this) {
            try {
                out.write(line.getBytes(StandardCharsets.UTF_8));
                out.write('\n');
                out.flush();
            } catch (IOException e) {
                throw AikoqlException.io("write failed: " + e.getMessage());
            }
        }
    }

    @Override
    public void close() {
        synchronized (this) {
            closed = true;
        }
        try {
            socket.close();
        } catch (IOException ignored) {
        }
    }

    /** Hands a line to the waiting reader, or queues it (the §3.3
     * late-response self-heal). */
    private void deliver(String line) {
        synchronized (this) {
            if (registeredDl != null) {
                registeredDl = null;
                pending = line;
                pendingDone = true;
                notifyAll();
            } else {
                lines.add(line);
                notifyAll();
            }
        }
    }

    private void readLoop() {
        char[] chunk = new char[4096];
        StringBuilder line = new StringBuilder();
        boolean capped = false;
        try {
            int n;
            while ((n = in.read(chunk)) != -1) {
                int start = 0;
                for (int i = 0; i < n; i++) {
                    if (chunk[i] == '\n') {
                        line.append(chunk, start, i - start);
                        if (line.length() > MAX_FRAME) {
                            // §19: a terminated over-cap line — refuse it
                            // before delivering (the stream is desynced).
                            capped = true;
                            break;
                        }
                        deliver(line.toString());
                        line.setLength(0);
                        start = i + 1;
                    }
                }
                if (capped) break;
                line.append(chunk, start, n - start);
                if (line.length() > MAX_FRAME) {
                    // §19: the unterminated tail past the cap — refuse
                    // mid-accumulation, before it can grow unboundedly.
                    capped = true;
                    break;
                }
            }
        } catch (IOException e) {
            // closed socket — same as EOF
        }
        synchronized (this) {
            closed = true;
            if (capped) {
                // Fail the waiter (or the next reader) with FRAME_TOO_LARGE,
                // never a bare EOF — the stream is desynced either way.
                pendingErr = AikoqlException.frameTooLarge();
                registeredDl = null;
                pending = null;
                pendingDone = false;
            } else if (registeredDl != null) {
                registeredDl = null;
                pending = null;
                pendingDone = true;
            }
            notifyAll(); // the deadline-less waiter must see the EOF too
        }
    }
}
