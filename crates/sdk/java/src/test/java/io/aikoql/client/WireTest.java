package io.aikoql.client;

// The wire-layer legs: ports of the TS suite's wire.test.ts (the Rust
// client's scripted-server suite). Each test drives a fake server through
// the frozen §3.3 semantics — id correlation, deadline → TIMEOUT, closed
// client → UNAVAILABLE, the version contract, the stream frame protocol,
// the transaction guard, and self-healing after a late frame.

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.ServerSocket;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.function.Executable;

class WireTest {

    /** Runs fn and returns the AikoqlException's code; fails otherwise. */
    private static String thrownCode(Executable fn) {
        try {
            fn.execute();
        } catch (AikoqlException e) {
            return e.getCode();
        } catch (Throwable t) {
            throw new AssertionError("expected AikoqlException, got " + t, t);
        }
        throw new AssertionError("expected AikoqlException, none thrown");
    }

    /** Runs fn and returns the AikoqlException itself; fails otherwise. */
    private static AikoqlException thrown(Executable fn) {
        try {
            fn.execute();
        } catch (AikoqlException e) {
            return e;
        } catch (Throwable t) {
            throw new AssertionError("expected AikoqlException, got " + t, t);
        }
        throw new AssertionError("expected AikoqlException, none thrown");
    }

    private static Connection dial(ServerSocket srv) {
        return AikoqlClient.dial("127.0.0.1:" + srv.getLocalPort());
    }

    @Test
    void initializeSendsTheHandshake() throws Exception {
        List<String> methods = Collections.synchronizedList(new ArrayList<>());
        try (ServerSocket srv = Scripted.scripted(req -> {
            methods.add(Json.jsonStr(req.fields.get("method")));
            return List.of(new Scripted.Frame(
                    Scripted.respond(Scripted.num(req.fields.get("id")), "0.2.0"), 0));
        })) {
            try (Connection c = dial(srv)) {
                c.initialize(null);
            }
        }
        assertEquals(1, methods.size());
        assertEquals("initialize", methods.get(0));
    }

    @Test
    void skipsStaleAndIdlessFrames() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of(
                new Scripted.Frame("{}", 0), // id-less push
                new Scripted.Frame("{\"id\":0,\"result\":{}}", 0), // stale
                new Scripted.Frame(
                        Scripted.respond(Scripted.num(req.fields.get("id")), "0.2.0"), 0)))) {
            try (Connection c = dial(srv)) {
                c.initialize(null);
            }
        }
    }

    @Test
    void protocolErrorOnForeignId() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of(
                new Scripted.Frame("{\"id\":99,\"result\":{}}", 0)))) {
            try (Connection c = dial(srv)) {
                assertEquals("PROTOCOL_ERROR", thrownCode(() -> c.initialize(null)));
            }
        }
    }

    @Test
    void rpcErrorEnvelope() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of(new Scripted.Frame(
                "{\"id\":" + Scripted.num(req.fields.get("id"))
                        + ",\"error\":{\"code\":\"NOT_FOUND\",\"message\":\"nope\"}}",
                0)))) {
            try (Connection c = dial(srv)) {
                assertEquals("NOT_FOUND", thrownCode(() -> c.initialize(null)));
            }
        }
    }

    @Test
    void noiseFramesAreSkipped() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of(
                new Scripted.Frame("not json", 0),
                new Scripted.Frame(
                        Scripted.respond(Scripted.num(req.fields.get("id")), "0.2.0"), 0)))) {
            try (Connection c = dial(srv)) {
                c.initialize(null);
            }
        }
    }

    @Test
    void deadlineMapsToRetryableTimeout() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of())) { // accepts, never answers
            try (Connection c = dial(srv)) {
                AikoqlException e = thrown(() -> {
                    AikoqlClient.withDeadline(200, dl -> {
                        c.initialize(dl);
                        return null;
                    });
                });
                assertEquals("TIMEOUT", e.getCode());
                assertTrue(e.isRetryable());
            }
        }
    }

    @Test
    void closedClientIsUnavailable() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of())) {
            Connection c = dial(srv);
            c.close();
            assertEquals(
                    "UNAVAILABLE",
                    thrownCode(() -> c.callTool("health", null, null)));
        }
    }

    @Test
    void tooOldServerFailsFastWithVersionMismatch() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of(new Scripted.Frame(
                Scripted.respond(Scripted.num(req.fields.get("id")), "0.0.1"), 0)))) {
            try (Connection c = dial(srv)) {
                assertEquals("VERSION_MISMATCH", thrownCode(() -> c.initialize(null)));
            }
        }
    }

    @Test
    void singleChunkStreamYieldsTheHeadAndEnds() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of(new Scripted.Frame(
                "{\"id\":" + Scripted.num(req.fields.get("id"))
                        + ",\"result\":{\"stream_id\":\"s1\",\"total_chunks\":1,"
                        + "\"results\":[{\"koid\":\"k1\"}]}}",
                0)))) {
            try (Connection c = dial(srv)) {
                List<Json.Value> chunks = new ArrayList<>();
                try (ResultSet rs = c.queryStream("MATCH p RETURN *", "", null)) {
                    while (rs.hasNext()) chunks.add(rs.next());
                }
                assertEquals(1, chunks.size());
                assertEquals("k1", Json.jsonStr(Json.dotGet(chunks.get(0), "results.0.koid")));
            }
        }
    }

    @Test
    void multiChunkStreamEndsOnDone() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of(
                new Scripted.Frame(
                        "{\"id\":" + Scripted.num(req.fields.get("id"))
                                + ",\"result\":{\"stream_id\":\"s1\",\"total_chunks\":2,"
                                + "\"results\":[{\"koid\":\"k1\"}]}}",
                        0),
                new Scripted.Frame(
                        "{\"method\":\"notifications/notify\",\"params\":{\"stream_id\":\"s1\","
                                + "\"chunk\":2,\"done\":true,\"results\":[{\"koid\":\"k2\"}]}}",
                        0)))) {
            try (Connection c = dial(srv)) {
                List<Json.Value> chunks = new ArrayList<>();
                try (ResultSet rs = c.queryStream("MATCH p RETURN *", "", null)) {
                    while (rs.hasNext()) chunks.add(rs.next());
                }
                assertEquals(2, chunks.size());
                assertEquals("k2", Json.jsonStr(Json.dotGet(chunks.get(1), "results.0.koid")));
            }
        }
    }

    @Test
    void abortedStreamReleasesTheConnection() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> {
            String method = Json.jsonStr(req.fields.get("method"));
            if (method.equals("aikoql/stream")) {
                return List.of(new Scripted.Frame(
                        "{\"id\":" + Scripted.num(req.fields.get("id"))
                                + ",\"result\":{\"stream_id\":\"s1\",\"total_chunks\":2,"
                                + "\"results\":[]}}",
                        0));
            }
            return List.of(new Scripted.Frame(
                    Scripted.toolResult(Scripted.num(req.fields.get("id")), "{\"status\":\"healthy\"}"),
                    0));
        })) {
            try (Connection c = dial(srv)) {
                Deadline dl = new Deadline();
                ResultSet rs = c.queryStream("MATCH p RETURN *", "", dl);
                Json.Value first = rs.next();
                assertEquals("s1", Json.jsonStr(Json.dotGet(first, "stream_id")));
                // Cancellation while blocked on a read: the abort surfaces
                // as the frozen TIMEOUT and the teardown releases the lock.
                Thread aborter = new Thread(() -> {
                    try {
                        Thread.sleep(100);
                    } catch (InterruptedException ignored) {
                    }
                    dl.abort();
                });
                aborter.start();
                assertEquals("TIMEOUT", thrownCode(rs::hasNext));
                aborter.join(2000);
                rs.close();
                assertEquals(
                        "healthy",
                        Json.jsonStr(Json.dotGet(c.callTool("health", null, null), "status")));
            }
        }
    }

    @Test
    void closedTransactionHandleRefusesFurtherUse() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> {
            long id = Scripted.num(req.fields.get("id"));
            String tool = Json.jsonStr(Json.dotGet(req, "params.name"));
            switch (tool) {
                case "txn_begin" -> {
                    return List.of(new Scripted.Frame(Scripted.toolResult(id, "{}"), 0));
                }
                case "txn_commit" -> {
                    return List.of(new Scripted.Frame(
                            Scripted.toolResult(id, "{\"results\":[],\"deduped\":false}"), 0));
                }
                default -> {
                    return List.of(new Scripted.Frame(
                            Scripted.toolError(id, "INTERNAL", "unexpected tool call " + tool),
                            0));
                }
            }
        })) {
            try (Connection c = dial(srv)) {
                Transaction tx = c.begin(null, null);
                Json.Value res = tx.commit(null);
                Json.Value deduped = Json.dotGet(res, "deduped");
                assertTrue(deduped instanceof Json.Bool b && !b.v());
                assertEquals(
                        "INVALID_ARGUMENT",
                        thrownCode(() -> tx.execute("create", "", null, null)));
            }
        }
    }

    @Test
    void lateResponseAfterTimeoutIsSkipped() throws Exception {
        int[] calls = {0};
        try (ServerSocket srv = Scripted.scripted(req -> {
            calls[0]++;
            long delay = calls[0] == 1 ? 300 : 0;
            return List.of(new Scripted.Frame(
                    Scripted.respond(Scripted.num(req.fields.get("id")), "0.2.0"), delay));
        })) {
            try (Connection c = dial(srv)) {
                assertEquals("TIMEOUT", thrownCode(() -> {
                    AikoqlClient.withDeadline(50, dl -> {
                        c.initialize(dl);
                        return null;
                    });
                }));
                // The late frame for request 1 arrives during this call and
                // is skipped (id 1 < id 2); the real answer for id 2 lands.
                c.initialize(null);
            }
        }
        assertTrue(calls[0] >= 2, "the second initialize never reached the server");
    }
}
