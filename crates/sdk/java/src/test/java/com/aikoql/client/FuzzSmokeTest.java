package io.aikoql.client;

import java.nio.charset.StandardCharsets;
import java.util.List;
import org.junit.jupiter.api.Test;

// The §14 smoke: every Jazzer target runs over the real-wire seed set
// (the Go §11 seeds, Java shapes) plus a deterministic pseudo-random
// sweep. Under the Jazzer engine the same entry points run with mutated
// inputs; this keeps a plain-JUnit check that they never crash.
public class FuzzSmokeTest {

    @Test
    public void seedsAndSweep() {
        run(FuzzParseRPCResponse::fuzzerTestOneInput, List.of(
                b("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"koid\":\"x\"}}"),
                b("{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32601,\"message\":\"nf\"}}"),
                b("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/notify\","
                        + "\"params\":{\"stream_id\":\"s\",\"done\":true}}"),
                b("{}")));
        run(FuzzParseMcpError::fuzzerTestOneInput, List.of(
                b("{\"code\":-32601,\"message\":\"method not found\"}"),
                b("{\"code\":\"-32601\",\"message\":\"string-encoded\"}"),
                b("{\"code\":\"FRAME_TOO_LARGE\",\"message\":\"over cap\"}"),
                b("{\"message\":\"no code\"}")));
        run(FuzzDecodeToolEnvelope::fuzzerTestOneInput, List.of(
                b("{\"content\":[{\"text\":\"{\\\"ok\\\":true,\\\"data\\\":{\\\"koid\\\":\\\"x\\\"}}\"}]}"),
                b("{\"content\":[{\"text\":\"{\\\"ok\\\":false,\\\"error\\\":"
                        + "{\\\"code\\\":\\\"NOT_FOUND\\\",\\\"message\\\":\\\"gone\\\"}}\"}]}"),
                b("{\"content\":[{\"text\":\"{\\\"ok\\\":true}\"}]}"),
                b("{\"content\":[]}"),
                b("{}")));
        run(FuzzDecodeStreamNotify::fuzzerTestOneInput, List.of(
                b("{\"method\":\"notifications/notify\","
                        + "\"params\":{\"stream_id\":\"s1\",\"done\":true}}"),
                b("{\"method\":\"notifications/notify\",\"params\":{\"stream_id\":\"s1\"}}"),
                b("{\"method\":\"notifications/notify\","
                        + "\"params\":{\"stream_id\":\"s1\",\"done\":false}}"),
                b("{}"),
                b("null")));
        run(FuzzVersionParser::fuzzerTestOneInput, List.of(
                b("0.2.0"), b("0.1.19"), b("2024-11-05"), b("x.y"), b("1.2.3.4.5"), b("")));
        run(FuzzErrorMapping::fuzzerTestOneInput, List.of(
                b("-32601"), b("\"-32601\""), b("FRAME_TOO_LARGE"), b("\"\""), b("")));
        // Two big-endian longs per input (zero-fill for short ones).
        run(FuzzRequestIDCorrelation::fuzzerTestOneInput, List.of(
                longs(1, 1), longs(1, 2), longs(2, 1), longs(0, 0), longs(-1, 5)));
        sweep();
    }

    /** A deterministic pseudo-random sweep — every target over 2000
     * inputs, so a crash surfaces without the Jazzer engine. */
    private static void sweep() {
        long x = 0x9E3779B97F4A7C15L;
        for (int i = 0; i < 2000; i++) {
            x = x * 6364136223846793005L + 1442695040888963407L;
            int len = (int) ((x >>> 33) % 49);
            byte[] data = new byte[len];
            for (int j = 0; j < len; j++) {
                x = x * 6364136223846793005L + 1442695040888963407L;
                data[j] = (byte) (x >>> 56);
            }
            FuzzParseRPCResponse.fuzzerTestOneInput(data);
            FuzzParseMcpError.fuzzerTestOneInput(data);
            FuzzDecodeToolEnvelope.fuzzerTestOneInput(data);
            FuzzDecodeStreamNotify.fuzzerTestOneInput(data);
            FuzzVersionParser.fuzzerTestOneInput(data);
            FuzzRequestIDCorrelation.fuzzerTestOneInput(data);
            FuzzErrorMapping.fuzzerTestOneInput(data);
        }
    }

    private static void run(java.util.function.Consumer<byte[]> target, List<byte[]> seeds) {
        for (byte[] seed : seeds) {
            target.accept(seed);
        }
    }

    private static byte[] b(String s) {
        return s.getBytes(StandardCharsets.UTF_8);
    }

    private static byte[] longs(long want, long got) {
        byte[] out = new byte[16];
        for (int i = 0; i < 8; i++) {
            out[i] = (byte) (want >>> (56 - 8 * i));
            out[8 + i] = (byte) (got >>> (56 - 8 * i));
        }
        return out;
    }
}
