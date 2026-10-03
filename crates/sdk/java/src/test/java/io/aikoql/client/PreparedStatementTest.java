package io.aikoql.client;

// The §3.6 prepared-statement legs: the ports of the Go suite's
// prepared_test.go — empty-query refusal, exact binding validation
// (count, presence, scalars), longest-first substitution, the closed
// handle guard, and one execute-through leg against a scripted server.

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.net.ServerSocket;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.function.Executable;

class PreparedStatementTest {

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

    private static String substitute(String query, List<String> params, Map<String, Object> bound) {
        try {
            return PreparedStatement.substitute(query, params, bound);
        } catch (AikoqlException e) {
            throw new AssertionError("unexpected " + e.getCode() + ": " + e.getMessage(), e);
        }
    }

    @Test
    void prepareRejectsEmptyQuery() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of())) {
            try (Connection c = AikoqlClient.dial("127.0.0.1:" + srv.getLocalPort())) {
                assertEquals("INVALID_ARGUMENT", thrownCode(() -> c.prepare("   ")));
            }
        }
    }

    @Test
    void bindWrongCountFails() {
        assertEquals(
                "INVALID_ARGUMENT",
                thrownCode(() -> PreparedStatement.substitute(
                        "MATCH :name", List.of("name"), Map.of())));
    }

    @Test
    void missingParameterFails() {
        assertEquals(
                "INVALID_ARGUMENT",
                thrownCode(() -> PreparedStatement.substitute(
                        "MATCH :name", List.of("name"), Map.of("other", 1))));
    }

    @Test
    void nonScalarFails() {
        assertEquals(
                "INVALID_ARGUMENT",
                thrownCode(() -> PreparedStatement.substitute(
                        "MATCH :name", List.of("name"), Map.of("name", List.of(1)))));
    }

    @Test
    void substitutesLongestFirst() {
        String q = substitute(
                "MATCH :name WHERE :name2 = :name",
                List.of("name", "name2"),
                Map.of("name", "a", "name2", 2));
        assertEquals("MATCH \"a\" WHERE 2 = \"a\"", q);
    }

    @Test
    void closedStatementRefusesExecute() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of())) {
            try (Connection c = AikoqlClient.dial("127.0.0.1:" + srv.getLocalPort())) {
                PreparedStatement ps = c.prepare("MATCH :x");
                ps.close();
                assertEquals(
                        "INVALID_ARGUMENT",
                        thrownCode(() -> ps.execute(Map.of("x", 1), null)));
            }
        }
    }

    @Test
    void executeSubstitutesAndRuns() throws Exception {
        List<String> queries = Collections.synchronizedList(new java.util.ArrayList<>());
        try (ServerSocket srv = Scripted.scripted(req -> {
            queries.add(Json.jsonStr(Json.dotGet(req, "params.arguments.query")));
            return List.of(new Scripted.Frame(
                    Scripted.toolResult(
                            Scripted.num(req.fields.get("id")), "{\"results\":[]}"),
                    0));
        })) {
            try (Connection c = AikoqlClient.dial("127.0.0.1:" + srv.getLocalPort())) {
                PreparedStatement ps = c.prepare("MATCH :name");
                ps.execute(Map.of("name", "k1"), null);
                // The statement stays reusable across bindings.
                ps.execute(Map.of("name", 7), null);
            }
        }
        assertEquals(List.of("MATCH \"k1\"", "MATCH 7"), List.copyOf(queries));
    }
}
