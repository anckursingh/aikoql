package io.aikoql.client;

// D-16 fault matrix (line wire): the §18 fault proxy sits between the SDK
// and a real server and mangles the newline-delimited JSON-RPC wire (§7 of
// the testing plan: every SDK passes the same matrix against the same
// misbehaving server). Frame accounting: client line #1 = initialize, #2 =
// the victim call, #3 = the follow-up (no HELLO/AUTH frames on the MCP
// wire). Mirrors crates/sdk/rust/tests/fault.rs on the line wire.
//
// The contracts GREEN must make hold (the proxy --wire line arm + the
// client's MAX_FRAME cap and closed latch):
//   drop-request/drop-response → victim TIMEOUT (retryable), follow-up ok
//   delay-response → tight deadline TIMEOUT; generous deadline ok
//   duplicate-response → both ok (id correlation skips the duplicate)
//   reorder-response → victim TIMEOUT (response held), follow-up ok
//   truncate-frame → victim TIMEOUT (the missing bytes never arrive),
//     follow-up ok (the wire is self-delimiting)
//   corrupt-frame → the line is noise per the frozen §3.3 semantics →
//     victim TIMEOUT, follow-up ok
//   inject-notification → both ok (an id-less frame is never a response)
//   inject-stale-response → both ok (stale id skipped)
//   close/half-close → victim ok, follow-up fails fast (never TIMEOUT),
//     the client latches closed → UNAVAILABLE from then on
//   slow-server → tight deadline TIMEOUT
//   oversized-response → FRAME_TOO_LARGE before buffering past the 1 MiB
//     cap (§19: a malicious server cannot cause unbounded client memory),
//     follow-up UNAVAILABLE (the stream is desynced — latched)

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotEquals;

import java.io.File;
import java.io.IOException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.function.Executable;

class FaultMatrixTest {

    private static final String TOKEN = "test-token";
    private static final String MCP_BIN = bin("aikoql-mcp");
    private static final String PROXY_BIN = bin("aikoql-fault-proxy");

    private final List<Process> procs = new ArrayList<>();
    private Path tmpDir;

    /** The debug binary, or "" (the test skips) — AIKOQL_MCP_BIN names it
     * on CI and the laptop smoke. */
    private static String bin(String name) {
        String env = System.getenv(name.equals("aikoql-mcp") ? "AIKOQL_MCP_BIN" : "AIKOQL_FAULT_PROXY");
        if (env != null && !env.isEmpty()) {
            return env;
        }
        for (String cand : new String[] {
                "../../../target/debug/" + name + ".exe",
                "../../../target/debug/" + name }) {
            if (new File(cand).isFile()) {
                return cand;
            }
        }
        return "";
    }

    @BeforeEach
    void setup() throws IOException {
        tmpDir = Files.createTempDirectory("aikoql-java-fault-");
    }

    @AfterEach
    void tearDown() throws IOException {
        killAll();
        if (System.getenv("AIKOQL_KEEP_TMP") == null && tmpDir != null && Files.exists(tmpDir)) {
            try (var walk = Files.walk(tmpDir)) {
                walk.sorted(java.util.Comparator.reverseOrder()).forEach(p -> p.toFile().delete());
            }
        }
    }

    private void killAll() {
        for (Process p : procs) {
            if (p.isAlive()) {
                p.destroyForcibly();
            }
            try {
                p.waitFor(5, TimeUnit.SECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }
        procs.clear();
    }

    private void spawn(String bin, List<String> args, File errFile) throws IOException {
        List<String> cmd = new ArrayList<>();
        cmd.add(bin);
        cmd.addAll(args);
        ProcessBuilder pb = new ProcessBuilder(cmd);
        pb.redirectOutput(ProcessBuilder.Redirect.DISCARD);
        pb.redirectError(errFile);
        procs.add(pb.start());
    }

    private static int freePort() throws IOException {
        try (ServerSocket s = new ServerSocket(0, 1, InetAddress.getLoopbackAddress())) {
            return s.getLocalPort();
        }
    }

    private static void waitUp(int port, Process proc, File errFile, String what) throws Exception {
        for (int i = 0; i < 100; i++) {
            if (!proc.isAlive()) {
                // IOException (not AssertionError): faultEnv's retry loop
                // only catches Exception — an Error would skip the retry.
                throw new IOException(what + " exited early (" + proc.exitValue() + "):\n"
                        + Files.readString(errFile.toPath()));
            }
            try (Socket s = new Socket("127.0.0.1", port)) {
                return;
            } catch (IOException e) {
                Thread.sleep(100);
            }
        }
        throw new IOException(what + " never listened on 127.0.0.1:" + port);
    }

    /** A real server + one fault-proxy instance (one fault mode) behind
     * the proxy address. The port-probe idiom races → 3 spawn attempts. */
    private String faultEnv(String mode, String... extra) throws Exception {
        for (int attempt = 0; attempt < 3; attempt++) {
            try {
                int srvPort = freePort();
                File srvErr = tmpDir.resolve("srv.err").toFile();
                // A fresh db path per attempt: the server REFUSES a path
                // an earlier attempt may have created (S-02).
                spawn(MCP_BIN, List.of("serve", tmpDir.resolve("db-" + attempt + ".aikoql").toString(),
                        "--listen", "127.0.0.1:" + srvPort,
                        "--tcp-token", TOKEN + "::admin"), srvErr);
                Process srv = procs.get(procs.size() - 1);
                waitUp(srvPort, srv, srvErr, "aikoql-mcp");

                int pxPort = freePort();
                File pxErr = tmpDir.resolve("proxy.err").toFile();
                List<String> args = new ArrayList<>(List.of(
                        "--listen", "127.0.0.1:" + pxPort,
                        "--target", "127.0.0.1:" + srvPort,
                        "--wire", "line", "--mode", mode));
                args.addAll(List.of(extra));
                spawn(PROXY_BIN, args, pxErr);
                Process px = procs.get(procs.size() - 1);
                waitUp(pxPort, px, pxErr, "aikoql-fault-proxy");
                return "127.0.0.1:" + pxPort;
            } catch (Exception e) {
                killAll();
                if (attempt == 2) {
                    throw e;
                }
            }
        }
        throw new AssertionError("unreachable");
    }

    private Connection dial(String addr) throws Exception {
        if (MCP_BIN.isEmpty() || PROXY_BIN.isEmpty()) {
            org.junit.jupiter.api.Assumptions.abort("aikoql-mcp/aikoql-fault-proxy not built — real-server fault matrix skipped");
        }
        Connection c = AikoqlClient.dial(addr).withToken(TOKEN);
        c.initialize(null);
        return c;
    }

    /** Runs fn and returns the error code ("" = ok). */
    private static String codeOf(Executable fn) {
        try {
            fn.execute();
            return "";
        } catch (AikoqlException e) {
            return e.getCode();
        } catch (Throwable t) {
            return "raw:" + t;
        }
    }

    /** The victim call under a deadline — the fault hits client line #2. */
    private static String victim(Connection c, long ms) {
        return codeOf(() -> AikoqlClient.withDeadline(ms,
                dl -> c.callTool("health", null, dl)));
    }

    /** The follow-up with no deadline — client line #3. */
    private static String followUp(Connection c) {
        return codeOf(() -> c.callTool("health", null, null));
    }

    private static void wantTimeout(String got) {
        assertEquals("TIMEOUT", got);
    }

    private static void wantOk(String got, String why) {
        assertEquals("", got, why);
    }

    /** The close/half-close contract: fail fast, then latch UNAVAILABLE. */
    private static void wantLatched(Connection c) {
        String first = followUp(c);
        assertNotEquals("", first, "follow-up should fail — the connection is closed");
        assertNotEquals("TIMEOUT", first, "follow-up must fail fast, never TIMEOUT");
        assertEquals("UNAVAILABLE", followUp(c), "latched: a dead conn never hangs");
    }

    @Test
    void dropRequest() throws Exception {
        String addr = faultEnv("drop-request", "--n", "2");
        Connection c = dial(addr);
        wantTimeout(victim(c, 200));
        wantOk(followUp(c), "the follow-up survives");
    }

    @Test
    void dropResponse() throws Exception {
        String addr = faultEnv("drop-response", "--n", "2");
        Connection c = dial(addr);
        wantTimeout(victim(c, 200));
        wantOk(followUp(c), "the follow-up survives");
    }

    @Test
    void delayResponse() throws Exception {
        String addr = faultEnv("delay-response", "--from", "2", "--delay-ms", "400");
        Connection c = dial(addr);
        wantTimeout(victim(c, 200));
        c.close();
        String addr2 = faultEnv("delay-response", "--from", "2", "--delay-ms", "400");
        Connection c2 = dial(addr2);
        wantOk(victim(c2, 2000), "a generous deadline absorbs the delay");
    }

    @Test
    void duplicateResponse() throws Exception {
        String addr = faultEnv("duplicate-response", "--n", "2");
        Connection c = dial(addr);
        wantOk(victim(c, 2000), "victim");
        wantOk(followUp(c), "the duplicate is a stale id — skipped");
    }

    @Test
    void reorderResponse() throws Exception {
        String addr = faultEnv("reorder-response", "--n", "2");
        Connection c = dial(addr);
        wantTimeout(victim(c, 200));
        wantOk(followUp(c), "request #3 releases response #2");
    }

    @Test
    void truncateResponse() throws Exception {
        String addr = faultEnv("truncate-response", "--n", "2", "--bytes", "8");
        Connection c = dial(addr);
        wantTimeout(victim(c, 200));
        wantOk(followUp(c), "the wire is self-delimiting");
    }

    @Test
    void corruptResponse() throws Exception {
        String addr = faultEnv("corrupt-response", "--n", "2");
        Connection c = dial(addr);
        wantTimeout(victim(c, 200));
        wantOk(followUp(c), "noise-skip, never a fast error");
    }

    @Test
    void injectNotification() throws Exception {
        String addr = faultEnv("inject-notification", "--after", "2");
        Connection c = dial(addr);
        wantOk(victim(c, 2000), "victim");
        wantOk(followUp(c), "an id-less frame is never a response");
    }

    @Test
    void injectStaleResponse() throws Exception {
        String addr = faultEnv("inject-stale-response", "--after", "2");
        Connection c = dial(addr);
        wantOk(victim(c, 2000), "victim");
        wantOk(followUp(c), "the replayed initialize response is stale");
    }

    @Test
    void closeAfter() throws Exception {
        String addr = faultEnv("close-after", "--n", "2");
        Connection c = dial(addr);
        wantOk(victim(c, 2000), "victim");
        wantLatched(c);
    }

    @Test
    void halfCloseAfter() throws Exception {
        String addr = faultEnv("half-close-after", "--n", "2");
        Connection c = dial(addr);
        wantOk(victim(c, 2000), "victim");
        wantLatched(c);
    }

    @Test
    void slowServer() throws Exception {
        String addr = faultEnv("slow-server", "--from", "2", "--bytes", "4", "--delay-ms", "25");
        Connection c = dial(addr);
        wantTimeout(victim(c, 200));
    }

    @Test
    void oversizedResponse() throws Exception {
        String addr = faultEnv("oversized-response", "--n", "2", "--claim", "67108864");
        Connection c = dial(addr);
        assertEquals("FRAME_TOO_LARGE", victim(c, 5000), "the 1 MiB cap, not 64 MiB");
        assertEquals("UNAVAILABLE", followUp(c), "the stream is desynced — latched");
    }
}
