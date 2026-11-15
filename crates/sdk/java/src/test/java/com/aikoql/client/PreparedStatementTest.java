package com.aikoql.client;

// The §3.6 prepared-statement legs: the ports of the Go suite's
// prepared_test.go — empty-query refusal, exact binding validation
// (count, presence, scalars), the closed
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

    @Test
    void prepareRejectsEmptyQuery() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of())) {
            try (Connection c = AikoqlClient.dial("127.0.0.1:" + srv.getLocalPort())) {
                assertEquals("INVALID_ARGUMENT", thrownCode(() -> c.prepare("   ")));
            }
        }
    }

    @Test
    void placeholdersInLiteralsAndCommentsAreText() throws Exception {
        try (ServerSocket srv = Scripted.scripted(req -> List.of())) {
            try (Connection c = AikoqlClient.dial("127.0.0.1:" + srv.getLocalPort())) {
                PreparedStatement ps = c.prepare(
                        "MATCH Doc WHERE tag == \"ns:prod\" AND name == :who // :nope\nRETURN *");
                // exactly one placeholder: binding :who alone is accepted by check
                PreparedStatement.check(List.of("who"), Map.of("who", "a"));
                assertEquals(
                        "INVALID_ARGUMENT",
                        thrownCode(() -> ps.execute(Map.of("who", "a", "nope", 1), null)));
            }
        }
    }

    @Test
    void bindWrongCountFails() {
        assertEquals(
                "INVALID_ARGUMENT",
                thrownCode(() -> PreparedStatement.check(List.of("name"), Map.of())));
    }

    @Test
    void missingParameterFails() {
        assertEquals(
                "INVALID_ARGUMENT",
                thrownCode(() -> PreparedStatement.check(List.of("name"), Map.of("other", 1))));
    }

    @Test
    void nonScalarFails() {
        assertEquals(
                "INVALID_ARGUMENT",
                thrownCode(() -> PreparedStatement.check(List.of("name"), Map.of("name", List.of(1)))));
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
            queries.add(Json.jsonStr(Json.dotGet(req, "params.arguments.query"))
                    + " " + Json.stringify(Json.dotGet(req, "params.arguments.params")));
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
        // the value travels as a parameter: it is never spliced into the query
        assertEquals(
                List.of("MATCH :name {\"name\":\"k1\"}", "MATCH :name {\"name\":7}"),
                List.copyOf(queries));
    }
}
