package com.aikoql.client;

// D-14: the shared conformance runner, java arm (§7, §23). This test pins
// only the CLI contract — the vectors carry the semantics. Mirrors
// crates/sdk/typescript/tests/conformance.test.ts and
// crates/sdk/rust/tests/conformance.rs.

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.List;
import org.junit.jupiter.api.Test;

class ConformancePinTest {

    // Native→"bash" spawns the WSL shim on Windows (its PATH has no java);
    // where.exe is a plain PATH walk and finds Git's bash. Prefer its Git
    // hit; plain "bash" elsewhere (CI Linux).
    private static String bashExe() throws IOException, InterruptedException {
        if (!System.getProperty("os.name").toLowerCase().contains("win")) {
            return "bash";
        }
        Process p = new ProcessBuilder("where.exe", "bash").start();
        String out = readAll(p.getInputStream());
        p.waitFor();
        List<String> hits = out.lines().map(String::trim).filter(l -> !l.isEmpty()).toList();
        return hits.stream()
                .filter(h -> h.toLowerCase().contains("git"))
                .findFirst()
                .orElse(hits.isEmpty() ? "bash" : hits.get(0));
    }

    // Walks up from the Maven module dir to the repo root (the anchor is
    // the runner itself, so a stale path fails loudly here).
    private static File repoRoot() {
        File dir = new File(System.getProperty("user.dir"));
        while (dir != null && !new File(dir, "scripts/sdk-conformance.sh").isFile()) {
            dir = dir.getParentFile();
        }
        return dir;
    }

    private static String readAll(InputStream in) throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        in.transferTo(out);
        return out.toString(StandardCharsets.UTF_8);
    }

    @Test
    void conformanceJavaArm() throws Exception {
        File root = repoRoot();
        assertNotNull(root, "repo root not found — D-14 RED");
        File script = new File(root, "scripts/sdk-conformance.sh");
        assertTrue(script.isFile(), "sdk-conformance runner missing at " + script + " — D-14 RED");
        String bin = System.getenv("AIKOQL_MCP_BIN");
        if (bin == null || bin.isEmpty()) {
            System.err.println("AIKOQL_MCP_BIN not set — real-server conformance skipped");
            return;
        }
        ProcessBuilder pb =
                new ProcessBuilder(bashExe(), script.getAbsolutePath(), "--language", "java");
        pb.environment().put("AIKOQL_MCP_BIN", bin);
        pb.redirectErrorStream(true);
        Process p = pb.start();
        String out = readAll(p.getInputStream());
        int code = p.waitFor();
        assertEquals(0, code, "sdk-conformance --language java failed (exit " + code + "):\n" + out);
    }
}
