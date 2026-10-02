package io.aikoql.client;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertTrue;

// The §14 Jazzer estate pin — the F-04 pattern: the estate's own pin
// detects a removed target, never a silent coverage loss. The seven targets
// mirror the Go §11 surfaces the Java wire layer shares; the transport's
// frame cap is covered by the §18 fault matrix instead (readLine is not a
// pure function here).
public class FuzzEstatePinTest {
    private static final String DIR = "src/test/java/io/aikoql/client/";

    @Test
    public void theSevenJazzerTargetsExist() throws IOException {
        for (String name : List.of(
                "FuzzParseRPCResponse",
                "FuzzParseMcpError",
                "FuzzDecodeToolEnvelope",
                "FuzzDecodeStreamNotify",
                "FuzzVersionParser",
                "FuzzRequestIDCorrelation",
                "FuzzErrorMapping")) {
            String src = Files.readString(Path.of(DIR + name + ".java"));
            assertTrue(src.contains("public static void fuzzerTestOneInput(byte[] data)"),
                    "§14 Jazzer target missing: " + name);
        }
    }
}
