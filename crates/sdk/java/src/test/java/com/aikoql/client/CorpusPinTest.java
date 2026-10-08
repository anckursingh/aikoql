package com.aikoql.client;

// The §16 pin: the cross-language golden corpus spec exists at
// sdk-fuzz-corpus/corpus.json and this SDK's column holds for every case.
// Removing a case id is a detected coverage loss; a column mismatch is a
// wire-behavior drift (or an undocumented divergence — document it in the
// spec's note and re-stamp). Real primitives run where they exist
// (Connection.parseVersion/classifyID/rpcError, ResultSet.decodeStreamNotify);
// the request/stream verdicts are restated inline at their source lines.
// Comparison is canonical-string: the built Value and the spec's java
// column both stringify, so key order and number shape come from the wire
// layer itself.

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.fail;

class CorpusPinTest {

    private static final List<String> CASE_IDS = List.of(
            "corp-v01", "corp-v02", "corp-v03", "corp-v04", "corp-v05",
            "corp-e01", "corp-e02", "corp-e03", "corp-e04", "corp-e05",
            "corp-r01", "corp-r02", "corp-n01",
            "corp-s01", "corp-s02", "corp-s03",
            "corp-c01", "corp-c02", "corp-c03", "corp-c04");

    private static final Set<String> SURFACES = Set.of(
            "version", "rpc_error", "response_id", "nonfinite", "notify", "correlation");

    private static Path repoRoot() {
        Path dir = Path.of(System.getProperty("user.dir")).toAbsolutePath();
        for (int i = 0; i < 8; i++) {
            if (Files.isRegularFile(dir.resolve("sdk-fuzz-corpus/corpus.json"))) return dir;
            dir = dir.getParent();
            if (dir == null) break; // climbed off the root without the spec
        }
        fail("the §16 corpus is absent (sdk-fuzz-corpus/corpus.json not found above "
                + System.getProperty("user.dir") + "): the D-16 golden-corpus slice is missing");
        return null;
    }

    private static Json.Obj corpus() throws IOException, AikoqlException {
        Path specPath = repoRoot().resolve("sdk-fuzz-corpus/corpus.json");
        String text = Files.readString(specPath);
        Json.Value v = Json.parse(text);
        if (!(v instanceof Json.Obj)) fail("the §16 corpus is not a JSON object");
        return (Json.Obj) v;
    }

    /** The §16 stream the notify verdicts run against. */
    private static final String STREAM_ID = "s1";

    /** This SDK's classification of one corpus input, as a Java structure
     * for Json.from. */
    private static Object evaluate(String surface, Json.Value input) throws AikoqlException {
        switch (surface) {
            case "version" -> {
                return boxed(Connection.parseVersion(((Json.Str) input).v()));
            }
            case "rpc_error" -> {
                String frame = ((Json.Str) input).v();
                Json.Value resp;
                try {
                    resp = Json.parse(frame);
                } catch (AikoqlException e) {
                    return "reject"; // the frame fails the parse — noise skip
                }
                Json.Value err = Json.dotGet(resp, "error");
                AikoqlException ex = Connection.rpcError(err);
                // LinkedHashMap: the canonical order is code then message.
                Map<String, Object> m = new LinkedHashMap<>();
                m.put("code", ex.getCode());
                m.put("message", ex.getMessage());
                return m;
            }
            case "response_id", "nonfinite" -> {
                String frame = ((Json.Str) input).v();
                Json.Value resp;
                try {
                    resp = Json.parse(frame);
                } catch (AikoqlException e) {
                    return "skip"; // noise frame (Connection.java:108-110)
                }
                Json.Value rid = Json.dotGet(resp, "id");
                if (!(rid instanceof Json.Num n)) return "skip";
                return corrName(Connection.classifyID(1, (long) n.v()));
            }
            case "notify" -> {
                String frame = ((Json.Str) input).v();
                Json.Value resp;
                try {
                    resp = Json.parse(frame);
                } catch (AikoqlException e) {
                    return skipPair();
                }
                // The stream loop's verdict, restated (ResultSet.java:91-95).
                ResultSet.StreamNotify sn = ResultSet.decodeStreamNotify(resp);
                if (sn == null || !sn.streamId().equals(STREAM_ID)) return skipPair();
                Map<String, Object> pair = new LinkedHashMap<>();
                pair.put("stream_id", sn.streamId());
                pair.put("done", sn.done());
                Map<String, Object> out = new LinkedHashMap<>();
                out.put("verdict", "yield");
                out.put("pair", pair);
                return out;
            }
            case "correlation" -> {
                long want = (long) ((Json.Num) Json.dotGet(input, "want")).v();
                long got = (long) ((Json.Num) Json.dotGet(input, "got")).v();
                return corrName(Connection.classifyID(want, got));
            }
            default -> throw new AssertionError("unknown surface " + surface);
        }
    }

    private static String corrName(Connection.Corr c) {
        return switch (c) {
            case SKIP -> "skip";
            case MATCH -> "match";
            case PROTOCOL -> "protocol";
        };
    }

    private static List<Integer> boxed(int[] vs) {
        var out = new java.util.ArrayList<Integer>(vs.length);
        for (int v : vs) out.add(v);
        return out;
    }

    private static Map<String, Object> skipPair() {
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("verdict", "skip");
        out.put("pair", null);
        return out;
    }

    @Test
    void corpusPin() throws Exception {
        Json.Obj spec = corpus();
        Json.Value casesV = Json.dotGet(spec, "cases");
        assertNotNull(casesV, "the §16 corpus has no cases array");
        assertTrue(casesV instanceof Json.Arr, "the §16 corpus cases is not an array");
        Json.Arr cases = (Json.Arr) casesV;

        var ids = new java.util.HashSet<String>();
        var surfaces = new java.util.HashSet<String>();
        for (Json.Value c : cases.items) {
            ids.add(Json.jsonStr(Json.dotGet(c, "id")));
            surfaces.add(Json.jsonStr(Json.dotGet(c, "surface")));
        }
        for (String id : CASE_IDS) {
            assertTrue(ids.contains(id), "case " + id + " is gone from the §16 corpus — a removed case is a coverage loss");
        }
        for (String s : SURFACES) {
            assertTrue(surfaces.contains(s), "surface " + s + " has no cases in the §16 corpus");
        }

        for (Json.Value c : cases.items) {
            String id = Json.jsonStr(Json.dotGet(c, "id"));
            String surface = Json.jsonStr(Json.dotGet(c, "surface"));
            Json.Value input = Json.dotGet(c, "input");
            Json.Value want = Json.dotGet(Json.dotGet(c, "expected"), "java");
            assertNotNull(want, "case " + id + " has no java column in the §16 corpus");
            Object got = evaluate(surface, input);
            assertEquals(Json.stringify(want), Json.stringify(Json.from(got)),
                    "case " + id + " (" + surface + "): java column drift");
        }
    }
}
