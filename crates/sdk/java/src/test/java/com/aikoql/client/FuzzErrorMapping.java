package io.aikoql.client;

import java.nio.charset.StandardCharsets;

// §14 L1: the frozen error-code mapping — the input is a raw JSON token
// (as a code arrives on the wire); embedded in a frame and mapped, it
// must come out as the parser's own string form, with the empty token
// mapping to INTERNAL (the Go §11 FuzzErrorMapping). Garbage that cannot
// parse is skipped — it cannot arrive on the wire.
public final class FuzzErrorMapping {
    public static void fuzzerTestOneInput(byte[] data) {
        String raw = new String(data, StandardCharsets.UTF_8);
        Json.Value frame;
        try {
            frame = Json.parse("{\"code\":" + raw + ",\"message\":\"m\"}");
        } catch (AikoqlException e) {
            return; // garbage cannot arrive on the wire
        }
        AikoqlException me = Connection.rpcError(frame);
        if (me.getCode().isEmpty()) throw new AssertionError("mapped code is empty");
        String want = Json.jsonStr(Json.dotGet(frame, "code"));
        if (want.isEmpty()) want = "INTERNAL";
        if (!me.getCode().equals(want)) {
            throw new AssertionError("code drifted: " + raw + " -> " + me.getCode()
                    + ", want " + want);
        }
        if (!me.getCode().equals(Connection.rpcError(frame).getCode())) {
            throw new AssertionError("mapping is not deterministic");
        }
    }

    private FuzzErrorMapping() {}
}
