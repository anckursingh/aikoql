package io.aikoql.client;

import java.nio.charset.StandardCharsets;

// §14 L1: the response-frame decode — a successful parse must stringify
// back to JSON that re-parses to the same value, and decoding is
// deterministic (the Go §11 properties, Java edition).
public final class FuzzParseRPCResponse {
    public static void fuzzerTestOneInput(byte[] data) {
        String s = new String(data, StandardCharsets.UTF_8);
        Json.Value v;
        try {
            v = Json.parse(s);
        } catch (AikoqlException e) {
            return; // malformed: nothing may escape
        }
        Json.Value again;
        try {
            again = Json.parse(s);
        } catch (AikoqlException e) {
            throw new AssertionError("second parse failed where the first succeeded", e);
        }
        if (!Json.jsonEq(v, again)) throw new AssertionError("decode is not deterministic: " + s);
        String out = Json.stringify(v);
        Json.Value round;
        try {
            round = Json.parse(out);
        } catch (AikoqlException e) {
            // The one lenient parse: a non-finite number ("1e999") re-emits
            // as the bare token Infinity, which is not JSON — Go rejects the
            // parse, Python accepts inf (§16 note). Only that may fail here.
            if (hasNonFinite(v)) return;
            throw new AssertionError("stringify escaped invalid JSON: " + out, e);
        }
        if (!Json.jsonEq(v, round)) throw new AssertionError("round-trip drifted: " + out);
    }

    private static boolean hasNonFinite(Json.Value v) {
        if (v instanceof Json.Num n) return Double.isInfinite(n.v());
        if (v instanceof Json.Arr a) {
            for (Json.Value item : a.items) {
                if (hasNonFinite(item)) return true;
            }
        }
        if (v instanceof Json.Obj o) {
            for (Json.Value item : o.fields.values()) {
                if (hasNonFinite(item)) return true;
            }
        }
        return false;
    }

    private FuzzParseRPCResponse() {}
}
