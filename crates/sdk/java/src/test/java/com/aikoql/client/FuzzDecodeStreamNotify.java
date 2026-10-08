package io.aikoql.client;

import java.nio.charset.StandardCharsets;

// §14 L1: the stream notify decode — a successful decode must round-trip
// through the wire form to the same (stream_id, done) pair, and decoding
// is deterministic (the Go §11 FuzzDecodeStreamChunk, over the parsed
// notify frame).
public final class FuzzDecodeStreamNotify {
    public static void fuzzerTestOneInput(byte[] data) {
        Json.Value v;
        try {
            v = Json.parse(new String(data, StandardCharsets.UTF_8));
        } catch (AikoqlException e) {
            return;
        }
        ResultSet.StreamNotify sn = ResultSet.decodeStreamNotify(v);
        if (sn == null) return; // not a notify frame
        Json.Value round;
        try {
            round = Json.parse(Json.stringify(Json.from(java.util.Map.of(
                    "method", "notifications/notify",
                    "params", java.util.Map.of(
                            "stream_id", sn.streamId(),
                            "done", sn.done())))));
        } catch (AikoqlException e) {
            throw new AssertionError("round-trip re-parse failed", e);
        }
        ResultSet.StreamNotify again = ResultSet.decodeStreamNotify(round);
        if (again == null || !again.streamId().equals(sn.streamId())
                || again.done() != sn.done()) {
            throw new AssertionError("round-trip drifted: " + sn + " vs " + again);
        }
        ResultSet.StreamNotify twice = ResultSet.decodeStreamNotify(v);
        if (twice == null || !twice.streamId().equals(sn.streamId())
                || twice.done() != sn.done()) {
            throw new AssertionError("decode is not deterministic");
        }
    }

    private FuzzDecodeStreamNotify() {}
}
