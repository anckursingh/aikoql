package com.aikoql.client;

// The Java adapter for the shared conformance runner (D-14, §7/§23). It
// executes the language-neutral vectors from tests/sdk-conformance/ (the
// §23 canonical workload + the §7 category dirs) and protocol/test-vectors/
// against a real aikoql-mcp server through this SDK, then checks every
// assert/assert_any/expect_error. The expected results are the vectors
// themselves — every SDK produces the same transcript. Mirrors the Rust
// adapter (crates/sdk/rust/src/bin/sdk-conformance.rs) arm for arm.
//
// Run through scripts/sdk-conformance.sh, or directly:
//
//   mvn -q -DskipTests compile && java -cp target/classes \
//       com.aikoql.client.SdkConformance --bin <aikoql-mcp> \
//       --vectors <dir> --protocol <dir> --token <token>

import java.io.IOException;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;

public final class SdkConformance {
    private SdkConformance() {}

    /** A check failure carries the message verbatim (Rust returns
     * Err(String) the same way); kernel errors are bare
     * IllegalStateException so run_vector can tell them apart. */
    private static final class CheckFailure extends IllegalStateException {
        CheckFailure(String message) {
            super(message);
        }
    }

    private static final class Runner {
        final String addr;
        final String token;
        Connection client;
        final Map<String, Object> vars = new HashMap<>();
        String last = "";

        Runner(String addr, String token) {
            this.addr = addr;
            this.token = token;
        }

        private Connection mustClient() {
            if (client == null) throw new IllegalStateException("vector must connect first");
            return client;
        }

        private void connect(String token) {
            Connection c = AikoqlClient.dial(addr).withToken(token);
            try {
                c.initialize(null);
            } catch (AikoqlException e) {
                c.close();
                throw e;
            }
            client = c;
        }

        /** ref resolves "$name" against the var map; anything else passes
         * through. */
        private String refKoid(Json.Value v) {
            String s = Json.jsonStr(v);
            if (s.startsWith("$")) {
                Object var = vars.get(s.substring(1));
                if (var instanceof String k) return k;
            }
            return s;
        }

        private Transaction txVar(Json.Value op) {
            String name = Json.jsonStr(Json.dotGet(op, "txn"));
            while (name.startsWith("$")) name = name.substring(1);
            Object var = vars.get(name);
            if (var instanceof Transaction tx) return tx;
            throw new IllegalStateException("var \"" + name + "\" is not an open txn");
        }

        private Json.Value runOp(Json.Obj op) {
            String name = Json.jsonStr(op.fields.get("op"));
            switch (name) {
                case "connect" -> {
                    String tok = Json.jsonStr(op.fields.get("token"));
                    connect(tok.isEmpty() ? token : tok);
                    return emptyObj();
                }
                case "close" -> {
                    if (client == null) throw new IllegalStateException("no client to close");
                    client.close();
                    // The client stays set: post-close calls fail with the
                    // frozen UNAVAILABLE (the Rust adapter's behavior).
                    return emptyObj();
                }
                case "health" -> {
                    return mustClient().health(null);
                }
                case "metrics" -> {
                    return mustClient().metrics(null);
                }
                case "remember" -> {
                    Json.Value rem = mustClient().remember(
                            Json.jsonStr(op.fields.get("type")),
                            null,
                            objToMap(op.fields.get("properties")),
                            null);
                    last = Json.jsonStr(Json.dotGet(rem, "koid"));
                    return rem;
                }
                case "update" -> {
                    String koid = or(refKoid(op.fields.get("koid")), last);
                    Json.Value rem = mustClient().remember(
                            Json.jsonStr(op.fields.get("type")),
                            koid,
                            objToMap(op.fields.get("properties")),
                            null);
                    last = Json.jsonStr(Json.dotGet(rem, "koid"));
                    return rem;
                }
                case "get" -> {
                    String koid = or(refKoid(op.fields.get("koid")), last);
                    return mustClient().get(koid, "", null);
                }
                case "delete" -> {
                    String koid = or(refKoid(op.fields.get("koid")), last);
                    Json.Value m = mustClient().forget(koid, "tombstone", "", null);
                    String k = Json.jsonStr(Json.dotGet(m, "koid"));
                    if (!k.isEmpty()) last = k;
                    return m;
                }
                case "query" -> {
                    Json.Value streamV = op.fields.get("stream");
                    if (streamV instanceof Json.Bool b && b.v()) {
                        Json.Arr chunks = new Json.Arr();
                        try (ResultSet rs =
                                mustClient().queryStream(Json.jsonStr(op.fields.get("query")), "", null)) {
                            while (rs.hasNext()) chunks.items.add(rs.next());
                        }
                        Json.Obj out = new Json.Obj();
                        out.fields.put("chunks", chunks);
                        return out;
                    }
                    return mustClient().aikoql(Json.jsonStr(op.fields.get("query")), "", null);
                }
                case "relate" -> {
                    Json.Value m = mustClient().relate(
                            refKoid(op.fields.get("from")),
                            refKoid(op.fields.get("to")),
                            Json.jsonStr(op.fields.get("rel_type")),
                            "",
                            null);
                    String k = Json.jsonStr(Json.dotGet(m, "koid"));
                    if (!k.isEmpty()) last = k;
                    return m;
                }
                case "traverse" -> {
                    Json.Value depthV = op.fields.get("depth");
                    long depth = depthV instanceof Json.Num n ? (long) n.v() : 1;
                    return mustClient().traverse(
                            refKoid(op.fields.get("koid")),
                            Json.jsonStr(op.fields.get("rel_type")),
                            "",
                            depth,
                            null);
                }
                case "find_similar" -> {
                    String text = Json.jsonStr(op.fields.get("text"));
                    Json.Value wfV = op.fields.get("wait_for_freshness_ms");
                    Long wf = wfV instanceof Json.Num n ? (long) n.v() : null;
                    Json.Value hits =
                            mustClient().findSimilar(text.isEmpty() ? null : text, wf, null);
                    Json.Obj out = new Json.Obj();
                    out.fields.put("results", hits);
                    return out;
                }
                case "begin" -> {
                    Transaction tx = mustClient().begin(null, null);
                    vars.put(Json.jsonStr(op.fields.get("as")), tx);
                    return obj(Map.of("txn_id", tx.id()));
                }
                case "execute" -> {
                    txVar(op).execute(
                            Json.jsonStr(op.fields.get("action")),
                            Json.jsonStr(op.fields.get("type")),
                            objToMap(op.fields.get("properties")),
                            null);
                    return emptyObj();
                }
                case "commit" -> {
                    return txVar(op).commit(null);
                }
                case "rollback" -> {
                    txVar(op).rollback(null);
                    return obj(Map.of("rolled_back", true));
                }
                case "explain" -> {
                    return mustClient().explain(refKoid(op.fields.get("koid")), "", null, null);
                }
                case "trace" -> {
                    return mustClient().trace(refKoid(op.fields.get("koid")), "", null);
                }
                case "discover_schema" -> {
                    return mustClient().discoverSchema(null);
                }
                default -> throw new IllegalStateException("op \"" + name + "\" has no adapter arm");
            }
        }

        private void capture(Json.Obj op, Json.Value result) {
            String asName = Json.jsonStr(op.fields.get("as"));
            if (asName.isEmpty()) return;
            String k = Json.jsonStr(Json.dotGet(result, "koid"));
            if (!k.isEmpty()) {
                vars.put(asName, k);
                return;
            }
            Json.Value first = Json.dotGet(result, "results");
            if (first instanceof Json.Arr a && !a.items.isEmpty()) {
                String fk = Json.jsonStr(Json.dotGet(a.items.get(0), "koid"));
                if (!fk.isEmpty()) vars.put(asName, fk);
            }
        }

        /** "$name" in an expected value resolves against the var map. */
        private Json.Value deref(Json.Value want) {
            if (want instanceof Json.Str s && s.v().startsWith("$")) {
                Object var = vars.get(s.v().substring(1));
                if (var instanceof String k) return new Json.Str(k);
            }
            return want;
        }

        private void check(Json.Obj op, Json.Value result) {
            Json.Value assertsV = op.fields.get("assert");
            if (assertsV instanceof Json.Obj asserts) {
                for (Map.Entry<String, Json.Value> e : asserts.fields.entrySet()) {
                    Json.Value got;
                    try {
                        got = pathGet(result, e.getKey());
                    } catch (IllegalStateException ex) {
                        throw new CheckFailure("assert " + e.getKey() + ": " + ex.getMessage());
                    }
                    Json.Value want = deref(e.getValue());
                    if (!Json.jsonEq(got, want)) {
                        throw new CheckFailure("assert " + e.getKey() + ": expected "
                                + Json.stringify(want) + ", got " + Json.stringify(got));
                    }
                }
            }
            Json.Value aaV = op.fields.get("assert_any");
            if (!(aaV instanceof Json.Obj aa)) return;
            String path = Json.jsonStr(aa.fields.get("path"));
            Json.Value items;
            try {
                items = pathGet(result, path);
            } catch (IllegalStateException ex) {
                throw new CheckFailure("assert_any " + path + ": " + ex.getMessage());
            }
            if (!(items instanceof Json.Arr a)) {
                throw new CheckFailure("assert_any " + path + ": not a list");
            }
            Json.Value m = aa.fields.get("match");
            if (m == null) throw new CheckFailure("assert_any: no match");
            boolean found = false;
            if (m instanceof Json.Obj wantMap) {
                outer:
                for (Json.Value item : a.items) {
                    if (!(item instanceof Json.Obj)) continue;
                    for (Map.Entry<String, Json.Value> p : wantMap.fields.entrySet()) {
                        Json.Value got;
                        try {
                            got = pathGet(item, p.getKey());
                        } catch (IllegalStateException ex) {
                            continue outer;
                        }
                        if (!Json.jsonEq(got, p.getValue())) continue outer;
                    }
                    found = true;
                    break;
                }
            } else {
                for (Json.Value item : a.items) {
                    if (Json.jsonEq(item, m)) {
                        found = true;
                        break;
                    }
                }
            }
            if (!found) {
                throw new CheckFailure(
                        "assert_any " + path + ": no element matches " + Json.stringify(m));
            }
        }

        /** The Rust dot_get: a dotted walk that errors, never nulls. */
        private static Json.Value pathGet(Json.Value obj, String path) {
            Json.Value cur = obj;
            for (String part : path.split("\\.")) {
                if (cur instanceof Json.Obj o) {
                    Json.Value next = o.fields.get(part);
                    if (next == null) throw new IllegalStateException("no key \"" + part + "\"");
                    cur = next;
                } else if (cur instanceof Json.Arr a) {
                    int idx;
                    try {
                        idx = Integer.parseInt(part);
                    } catch (NumberFormatException e) {
                        throw new IllegalStateException("no index \"" + part + "\"");
                    }
                    if (idx < 0 || idx >= a.items.size()) {
                        throw new IllegalStateException("no index \"" + part + "\"");
                    }
                    cur = a.items.get(idx);
                } else {
                    throw new IllegalStateException("cannot descend into "
                            + Json.stringify(cur) + " at \"" + part + "\"");
                }
            }
            return cur;
        }

        private void runVector(List<Json.Value> ops) {
            connect(token);
            vars.clear();
            last = "";
            for (int i = 0; i < ops.size(); i++) {
                Json.Value opv = ops.get(i);
                if (!(opv instanceof Json.Obj op)) {
                    throw new CheckFailure("op " + i + ": not an object");
                }
                String opName = Json.jsonStr(op.fields.get("op"));
                String expect = Json.jsonStr(op.fields.get("expect_error"));
                try {
                    Json.Value result = runOp(op);
                    if (!expect.isEmpty()) {
                        throw new CheckFailure("op " + i + " " + opName + ": expected error "
                                + expect + ", none raised");
                    }
                    capture(op, result);
                    try {
                        check(op, result);
                    } catch (CheckFailure e) {
                        throw new CheckFailure("op " + i + " " + opName + ": " + e.getMessage());
                    }
                } catch (CheckFailure e) {
                    throw e;
                } catch (AikoqlException e) {
                    String code = mapCode(e.getCode());
                    if (expect.equals(code)) continue;
                    throw new CheckFailure("op " + i + " " + opName + ": expected error " + expect
                            + ", got " + code + ": " + e.getMessage());
                } catch (IllegalStateException e) {
                    String code = e.getMessage() == null ? "" : e.getMessage();
                    if (expect.equals(code)) continue;
                    throw new CheckFailure("op " + i + " " + opName + ": expected error " + expect
                            + ", got " + code);
                }
            }
        }
    }

    private static Json.Obj emptyObj() {
        return new Json.Obj();
    }

    private static Json.Obj obj(Map<String, Object> m) {
        return (Json.Obj) Json.from(m);
    }

    private static String or(String a, String b) {
        return a.isEmpty() ? b : a;
    }

    /** Wire surfaces → SDK-012 codes: the server's -32001 token rejection
     * is AUTHENTICATION_FAILED to the caller. */
    private static String mapCode(String code) {
        return code.equals("-32001") ? "AUTHENTICATION_FAILED" : code;
    }

    private static Map<String, Object> objToMap(Json.Value v) {
        Map<String, Object> out = new LinkedHashMap<>();
        if (v instanceof Json.Obj o) {
            for (Map.Entry<String, Json.Value> e : o.fields.entrySet()) {
                out.put(e.getKey(), toPlain(e.getValue()));
            }
        }
        return out;
    }

    private static Object toPlain(Json.Value v) {
        if (v instanceof Json.Str s) return s.v();
        if (v instanceof Json.Num n) return n.v();
        if (v instanceof Json.Bool b) return b.v();
        if (v instanceof Json.Obj) return objToMap(v);
        if (v instanceof Json.Arr a) {
            List<Object> l = new ArrayList<>();
            for (Json.Value item : a.items) l.add(toPlain(item));
            return l;
        }
        return null;
    }

    private record VectorFile(String name, List<Json.Value> operations) {}

    private static List<VectorFile> loadVectors(String dir) throws IOException {
        List<Path> paths;
        try (Stream<Path> walk = Files.walk(Path.of(dir))) {
            paths = walk.filter(p -> p.toString().endsWith(".json")).sorted().toList();
        }
        List<VectorFile> out = new ArrayList<>();
        for (Path p : paths) {
            Json.Value parsed;
            try {
                parsed = Json.parse(Files.readString(p));
            } catch (IOException | AikoqlException e) {
                throw new IOException(p + ": " + e.getMessage(), e);
            }
            if (!(parsed instanceof Json.Obj o)) throw new IOException(p + ": not an object");
            Json.Value ops = o.fields.get("operations");
            out.add(new VectorFile(
                    Json.jsonStr(o.fields.get("name")),
                    ops instanceof Json.Arr a ? a.items : List.of()));
        }
        return out;
    }

    private record Server(Process child, String addr, Path dir) {}

    /** The integration_test pattern: a probed free port and a db path that
     * does not exist (the server auto-creates it as aikoql-v2). */
    private static Server spawnServer(String bin, String token) throws IOException {
        int port;
        try (ServerSocket probe = new ServerSocket(0, 50, InetAddress.getByName("127.0.0.1"))) {
            port = probe.getLocalPort();
        }
        Path dir = Files.createTempDirectory("conformance-");
        Path db = dir.resolve("db.aikoql");
        String addr = "127.0.0.1:" + port;
        Process child = new ProcessBuilder(
                        bin,
                        "serve",
                        db.toAbsolutePath().toString(),
                        "--listen",
                        addr,
                        "--tcp-token",
                        token + "::admin")
                // Stdout/Stderr stay null: an inherited pipe would let an
                // orphaned server hold a pipe-capturing parent open on
                // failure.
                .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                .redirectError(ProcessBuilder.Redirect.DISCARD)
                .start();
        long deadline = System.currentTimeMillis() + 15000;
        while (System.currentTimeMillis() < deadline) {
            try (Socket s = new Socket()) {
                s.connect(new InetSocketAddress("127.0.0.1", port), 200);
                return new Server(child, addr, dir);
            } catch (IOException e) {
                try {
                    Thread.sleep(50);
                } catch (InterruptedException ie) {
                    Thread.currentThread().interrupt();
                    throw new IOException("interrupted waiting for the server", ie);
                }
            }
        }
        child.destroy();
        try {
            child.waitFor();
        } catch (InterruptedException ignored) {
        }
        throw new IOException("server did not come up on " + addr);
    }

    private static void sweep(Path dir) {
        try (Stream<Path> walk = Files.walk(dir)) {
            for (Path p : walk.sorted(Comparator.reverseOrder()).toList()) {
                try {
                    Files.deleteIfExists(p);
                } catch (IOException ignored) {
                }
            }
        } catch (IOException ignored) {
        }
    }

    private static void run(String bin, String vectors, String protocol, String token)
            throws Exception {
        Server srv = spawnServer(bin, token);
        int vectorsRun = 0;
        int opsRun = 0;
        try {
            Runner r = new Runner(srv.addr(), token);
            for (String dir : new String[] {protocol, vectors}) {
                for (VectorFile vf : loadVectors(dir)) {
                    try {
                        r.runVector(vf.operations());
                    } catch (IllegalStateException e) {
                        throw new Exception(vf.name() + ": " + e.getMessage(), e);
                    }
                    vectorsRun++;
                    opsRun += vf.operations().size();
                    System.out.println("  ok " + vf.name() + " (" + vf.operations().size() + " ops)");
                }
            }
            System.out.println(
                    "sdk-conformance (java): " + vectorsRun + " vectors, " + opsRun
                            + " ops — all passed");
        } finally {
            // Kills the server and sweeps its temp dir on EVERY exit path —
            // the Rust adapter's ServerGuard, as a finally.
            srv.child().destroy();
            try {
                srv.child().waitFor();
            } catch (InterruptedException ignored) {
            }
            sweep(srv.dir());
        }
    }

    public static void main(String[] args) {
        String bin = "";
        String vectors = "";
        String protocol = "";
        String token = "conformance";
        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--bin" -> bin = args[++i];
                case "--vectors" -> vectors = args[++i];
                case "--protocol" -> protocol = args[++i];
                case "--token" -> token = args[++i];
                default -> {
                    System.err.println("unknown arg: " + args[i]);
                    System.exit(1);
                }
            }
        }
        try {
            run(bin, vectors, protocol, token);
        } catch (Exception e) {
            System.err.println("sdk-conformance (java): " + e.getMessage());
            System.exit(1);
        }
    }
}
