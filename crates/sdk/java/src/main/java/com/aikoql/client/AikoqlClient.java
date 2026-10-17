package com.aikoql.client;

import java.util.concurrent.ScheduledThreadPoolExecutor;
import java.util.concurrent.TimeUnit;

// The entry point (§25 Phase 5): dial opens a Connection; withDeadline is
// the response-deadline helper. Mirrors the module-level surface of the TS
// SDK (index.ts).
public final class AikoqlClient {
    /** The oldest aikoql-mcp server this SDK will talk to (the ND-12
     * version contract, mirrored from the Python, Go, Rust and TS SDKs). */
    public static final String MIN_SERVER_VERSION = "0.2.2";
    /** The SDK's own package version, advertised as the client identity.
     * Injected by Maven resource filtering from pom.xml (version.properties)
     * — not a restated literal; "dev" outside a Maven build. */
    public static final String VERSION = loadVersion();

    static final int DIAL_TIMEOUT_MS = 5000;

    private static String loadVersion() {
        try (java.io.InputStream in = AikoqlClient.class.getResourceAsStream("version.properties")) {
            if (in != null) {
                var props = new java.util.Properties();
                props.load(in);
                String v = props.getProperty("version", "");
                if (!v.isEmpty() && !v.startsWith("${")) {
                    return v;
                }
            }
        } catch (java.io.IOException e) {
            // no resource — the un-filtered build falls through to "dev"
        }
        return "dev";
    }

    private static final ScheduledThreadPoolExecutor TIMER =
            new ScheduledThreadPoolExecutor(1, r -> {
                Thread t = new Thread(r, "aikoql-deadline");
                t.setDaemon(true);
                return t;
            });

    private AikoqlClient() {}

    /** Opens a TCP connection ("host:port"). The handshake is separate
     * (initialize) so a caller can dial first and authenticate later;
     * every aikoql-mcp TCP server requires the token. */
    public static Connection dial(String addr) {
        return new Connection(addr);
    }

    /** Runs fn under a response deadline: elapsed → the frozen retryable
     * TIMEOUT. A late response afterwards is harmless — id correlation
     * skips it on the next call (self-healing). */
    public static <T> T withDeadline(long ms, DeadlineFn<T> fn) throws AikoqlException {
        Deadline dl = new Deadline();
        var f = TIMER.schedule(dl::abort, ms, TimeUnit.MILLISECONDS);
        try {
            return fn.apply(dl);
        } catch (AikoqlException e) {
            if (dl.isAborted()) throw AikoqlException.deadline();
            throw e;
        } finally {
            f.cancel(false);
        }
    }

    @FunctionalInterface
    public interface DeadlineFn<T> {
        T apply(Deadline dl) throws AikoqlException;
    }
}
