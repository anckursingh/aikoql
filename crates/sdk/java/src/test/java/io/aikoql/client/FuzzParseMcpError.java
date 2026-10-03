package io.aikoql.client;

import java.nio.charset.StandardCharsets;

// §14 L1: the error-frame decode — a decoded error frame maps to a
// non-empty code and its message verbatim, deterministically (the Go §11
// FuzzParseMcpError, over the parsed-value pipeline instead of a struct).
public final class FuzzParseMcpError {
    public static void fuzzerTestOneInput(byte[] data) {
        Json.Value v;
        try {
            v = Json.parse(new String(data, StandardCharsets.UTF_8));
        } catch (AikoqlException e) {
            return;
        }
        AikoqlException me = Connection.rpcError(v);
        if (me.getCode().isEmpty()) throw new AssertionError("mapped code is empty");
        if (v instanceof Json.Obj o) {
            // The frozen Java truth (the TS SDK agrees): getMessage() carries
            // the [code] decoration itself, where the Go/Python SDKs keep the
            // message raw — a documented divergence for the §16 corpus.
            String want = Json.jsonStr(o.fields.get("message"));
            if (!me.getMessage().equals("[" + me.getCode() + "] " + want)) {
                throw new AssertionError("message altered: " + me.getMessage()
                        + " vs [" + me.getCode() + "] " + want);
            }
        }
        AikoqlException again = Connection.rpcError(v);
        if (!me.getCode().equals(again.getCode())
                || !me.getMessage().equals(again.getMessage())) {
            throw new AssertionError("mapping is not deterministic");
        }
    }

    private FuzzParseMcpError() {}
}
