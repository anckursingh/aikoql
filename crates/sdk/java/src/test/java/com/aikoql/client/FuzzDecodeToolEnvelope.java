package com.aikoql.client;

import java.nio.charset.StandardCharsets;

// §14 L1: the tools/call envelope decode — never panics (a classified
// AikoqlException is the only failure mode), and the same input always
// yields the same payload or the same mapped error (determinism).
public final class FuzzDecodeToolEnvelope {
    public static void fuzzerTestOneInput(byte[] data) {
        Json.Value v;
        try {
            v = Json.parse(new String(data, StandardCharsets.UTF_8));
        } catch (AikoqlException e) {
            return;
        }
        Json.Value p1 = null;
        AikoqlException e1 = null;
        try {
            p1 = Connection.decodeToolEnvelope("fuzz", v);
        } catch (AikoqlException e) {
            e1 = e;
        }
        try {
            Json.Value p2 = Connection.decodeToolEnvelope("fuzz", v);
            if (e1 != null || !Json.jsonEq(p1, p2)) {
                throw new AssertionError("decode is not deterministic");
            }
        } catch (AikoqlException e2) {
            if (e1 == null || !e1.getCode().equals(e2.getCode())
                    || !e1.getMessage().equals(e2.getMessage())) {
                throw new AssertionError("error classification is not deterministic: "
                        + (e1 == null ? "ok" : e1.getCode() + " " + e1.getMessage())
                        + " vs " + e2.getCode() + " " + e2.getMessage());
            }
        }
    }

    private FuzzDecodeToolEnvelope() {}
}
