package com.aikoql.client;

// The SDK over TLS against the real server started with --tls-cert /
// --tls-key (the MCP crate's test certificates, SAN IP:127.0.0.1):
// AikoqlClient.dial(addr, an SSLContext trusting the test CA) initializes
// and round-trips a write; a context trusting another CA is refused at the
// dial, before the token is sent. Skipped without AIKOQL_MCP_BIN.

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.io.File;
import java.io.FileInputStream;
import java.io.InputStream;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.KeyStore;
import java.security.cert.CertificateFactory;
import java.util.Map;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManagerFactory;
import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.Test;

class TlsTest {
    private static final Path FX = Path.of("..", "..", "services", "api", "mcp", "tests", "fixtures", "tls");

    private static SSLContext trusting(String ca) throws Exception {
        KeyStore ks = KeyStore.getInstance(KeyStore.getDefaultType());
        ks.load(null, null);
        try (InputStream in = new FileInputStream(FX.resolve(ca).toFile())) {
            ks.setCertificateEntry("ca", CertificateFactory.getInstance("X.509").generateCertificate(in));
        }
        TrustManagerFactory tmf = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm());
        tmf.init(ks);
        SSLContext ctx = SSLContext.getInstance("TLS");
        ctx.init(null, tmf.getTrustManagers(), null);
        return ctx;
    }

    /** A TLS server on a free port; the process and its store are torn down by the caller. */
    private static final class Server implements AutoCloseable {
        final Process proc;
        final File dir;
        final String addr;

        Server() throws Exception {
            String bin = System.getenv("AIKOQL_MCP_BIN");
            Assumptions.assumeTrue(bin != null, "AIKOQL_MCP_BIN not set — TLS real-server test skipped");
            int port;
            try (ServerSocket probe = new ServerSocket(0)) {
                port = probe.getLocalPort();
            }
            addr = "127.0.0.1:" + port;
            dir = Files.createTempDirectory("java-tls-").toFile();
            proc = new ProcessBuilder(bin, "serve", new File(dir, "kb").getPath(),
                            "--listen", addr, "--tcp-token", "java-tls::admin",
                            "--tls-cert", FX.resolve("server.pem").toString(),
                            "--tls-key", FX.resolve("server.key").toString())
                    .redirectErrorStream(true)
                    .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                    .start();
            long deadline = System.currentTimeMillis() + 15_000;
            while (true) {
                try (Socket s = new Socket("127.0.0.1", port)) {
                    return;
                } catch (Exception e) {
                    if (System.currentTimeMillis() > deadline) throw new IllegalStateException("server never came up on " + addr);
                    Thread.sleep(100);
                }
            }
        }

        @Override
        public void close() throws Exception {
            proc.destroyForcibly().waitFor();
            try (var walk = Files.walk(dir.toPath())) {
                walk.sorted(java.util.Comparator.reverseOrder()).map(Path::toFile).forEach(File::delete);
            }
        }
    }

    @Test
    void mcpOverTls() throws Exception {
        try (Server srv = new Server();
                Connection c = AikoqlClient.dial(srv.addr, trusting("ca.pem")).withToken("java-tls")) {
            c.initialize(null);
            Json.Value put = c.callTool("remember",
                    Map.of("subject", "admin", "type_name", "note", "properties", Map.of("n", 7)), null);
            String koid = Json.jsonStr(Json.dotGet(put, "koid"));
            Json.Value got = c.callTool("get", Map.of("subject", "admin", "koid", koid), null);
            assertEquals("7", Json.stringify(Json.dotGet(got, "properties.n")));
        }
    }

    @Test
    void anUntrustedServerIsRefused() throws Exception {
        try (Server srv = new Server()) {
            SSLContext other = trusting("other-ca.pem");
            assertThrows(AikoqlException.class, () -> AikoqlClient.dial(srv.addr, other));
        }
    }
}
