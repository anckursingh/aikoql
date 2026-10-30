package com.aikoql.client;

// MRFC-0080 R5b — the client follows a NOT_LEADER leader hint (the Java
// leg; Go/Rust/Python/TS carry the same pair). A fenced server refuses a
// write with NOT_LEADER and the newer leader's client address; the call is
// retried once against that leader over a fresh, initialized connection,
// and the client stays there.

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.ServerSocket;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class LeaderHintTest {
    /** The envelope a fenced server answers a write with (R4d). */
    private static String notLeader(long id, String leader) {
        String hint = leader == null ? "" : ",\"leader\":\"" + leader + "\"";
        String text = "{\"ok\":false,\"error\":{\"code\":\"NOT_LEADER\",\"message\":\"not leader\","
                + "\"retryable\":true,\"suggestion\":\"\"" + hint + "}}";
        return "{\"id\":" + id + ",\"result\":{\"content\":[{\"text\":"
                + Json.stringify(new Json.Str(text)) + "}],\"isError\":true}}";
    }

    @Test
    void notLeaderFollowsTheLeaderHint() throws Exception {
        List<String> saw = Collections.synchronizedList(new ArrayList<>());
        try (ServerSocket leader = Scripted.scripted(req -> {
                    String method = Json.jsonStr(req.fields.get("method"));
                    saw.add(method);
                    long id = Scripted.num(req.fields.get("id"));
                    return List.of(new Scripted.Frame(method.equals("initialize")
                            ? Scripted.respond(id, AikoqlClient.MIN_SERVER_VERSION)
                            : Scripted.toolResult(id, "{\"koid\":\"ko-1\"}"), 0));
                });
                ServerSocket fenced = Scripted.scripted(req -> {
                    long id = Scripted.num(req.fields.get("id"));
                    return List.of(new Scripted.Frame(
                            Json.jsonStr(req.fields.get("method")).equals("initialize")
                                    ? Scripted.respond(id, AikoqlClient.MIN_SERVER_VERSION)
                                    : notLeader(id, "127.0.0.1:" + leader.getLocalPort()), 0));
                })) {
            try (Connection c = AikoqlClient.dial("127.0.0.1:" + fenced.getLocalPort()).withToken("t")) {
                c.initialize(null);
                Json.Value got = c.callTool("remember", Map.of("type_name", "note"), null);
                assertEquals("ko-1", Json.jsonStr(Json.dotGet(got, "koid")));
                c.callTool("remember", Map.of("type_name", "note"), null);
            }
        }
        assertEquals(List.of("initialize", "tools/call", "tools/call"), saw);
    }

    @Test
    void notLeaderWithoutAHintIsThrown() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of(new Scripted.Frame(
                notLeader(Scripted.num(req.fields.get("id")), null), 0)))) {
            try (Connection c = AikoqlClient.dial("127.0.0.1:" + srv.getLocalPort())) {
                AikoqlException e = assertThrows(AikoqlException.class,
                        () -> c.callTool("remember", Map.of("type_name", "note"), null));
                assertEquals("NOT_LEADER", e.getCode());
                assertTrue(e.isRetryable());
                assertNull(e.getLeader());
            }
        }
    }
}
