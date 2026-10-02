package io.aikoql.client;

// §14 L3: the frozen §3.3 correlation rules restated independently —
// smaller ids skip, larger ids are PROTOCOL_ERROR, equal ids match (the
// Go §11 FuzzRequestIDCorrelation). The input is two big-endian longs;
// short inputs zero-fill, so 0/0 and negative ids (a leading 0x80+ byte)
// are reachable.
public final class FuzzRequestIDCorrelation {
    public static void fuzzerTestOneInput(byte[] data) {
        long want = 0;
        long got = 0;
        for (int i = 0; i < 8; i++) {
            if (i < data.length) want = (want << 8) | (data[i] & 0xff);
            if (i + 8 < data.length) got = (got << 8) | (data[i + 8] & 0xff);
        }
        Connection.Corr c = Connection.classifyID(want, got);
        if (got < want && c != Connection.Corr.SKIP) {
            throw new AssertionError("smaller id must skip: " + want + " vs " + got);
        }
        if (got > want && c != Connection.Corr.PROTOCOL) {
            throw new AssertionError("larger id must be PROTOCOL_ERROR: " + want + " vs " + got);
        }
        if (got == want && c != Connection.Corr.MATCH) {
            throw new AssertionError("equal ids must match: " + want + " vs " + got);
        }
    }

    private FuzzRequestIDCorrelation() {}
}
