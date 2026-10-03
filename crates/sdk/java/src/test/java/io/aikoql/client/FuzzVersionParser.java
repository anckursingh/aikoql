package io.aikoql.client;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;

// §14 L3: the frozen mirror of Python's int(seg) semantics (signed
// segments ARE numeric there, so no lower bound), plus determinism and
// the irreflexivity of versionLess (the Go §11 FuzzVersionParser).
public final class FuzzVersionParser {
    public static void fuzzerTestOneInput(byte[] data) {
        String v = new String(data, StandardCharsets.UTF_8);
        int[] p = Connection.parseVersion(v);
        if (Connection.versionLess(p, p)) {
            throw new AssertionError("versionLess is not irreflexive for " + v);
        }
        if (!Arrays.equals(p, Connection.parseVersion(v))) {
            throw new AssertionError("parse is not deterministic for " + v);
        }
    }

    private FuzzVersionParser() {}
}
