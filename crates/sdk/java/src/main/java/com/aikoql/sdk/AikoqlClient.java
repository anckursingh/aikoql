// Aikoql Java SDK — MCP JSON-RPC client over TCP.
// Zero dependencies: compiles with plain javac, uses manual JSON.
//
// Usage:
//   AikoqlClient db = new AikoqlClient("127.0.0.1", 9090);
//   db.initialize();
//   String ko = db.remember("fact", "{\"x\": 1}");

package com.aikoql.sdk;

import java.io.*;
import java.net.Socket;
import java.nio.charset.StandardCharsets;

/** Minimal zero-dependency MCP JSON-RPC client for aikoql. */
public class AikoqlClient implements AutoCloseable {
    private final String host;
    private final int port;
    private Socket socket;
    private BufferedReader reader;
    private BufferedWriter writer;
    private int nextId;

    public AikoqlClient(String host, int port) {
        this.host = host;
        this.port = port;
    }

    public void connect() throws IOException {
        socket = new Socket(host, port);
        reader = new BufferedReader(new InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8));
        writer = new BufferedWriter(new OutputStreamWriter(socket.getOutputStream(), StandardCharsets.UTF_8));
    }

    public void initialize() throws IOException {
        request("initialize",
            "{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"aikoql-java-sdk\",\"version\":\"0.1\"}}");
    }

    @Override
    public void close() throws IOException {
        if (socket != null) socket.close();
    }

    // ---- Minimal JSON helpers ----------------------------------------------

    private static String esc(String s) {
        return s.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    private static String jsonObj(String... kvs) {
        StringBuilder sb = new StringBuilder("{");
        for (int i = 0; i < kvs.length; i += 2) {
            if (i > 0) sb.append(",");
            sb.append("\"").append(kvs[i]).append("\":").append(kvs[i + 1]);
        }
        return sb.append("}").toString();
    }

    private static String jsonStr(String s) {
        return "\"" + esc(s) + "\"";
    }

    private static String jsonNum(long n) {
        return Long.toString(n);
    }

    // ---- MCP transport -----------------------------------------------------

    private String request(String method, String paramsJson) throws IOException {
        int id = ++nextId;
        String frame = jsonObj(
            "jsonrpc", jsonStr("2.0"),
            "id", jsonNum(id),
            "method", jsonStr(method),
            "params", paramsJson
        );
        writer.write(frame + "\n");
        writer.flush();
        while (true) {
            String line = reader.readLine();
            if (line == null) throw new IOException("connection closed");
            if (line.isEmpty()) continue;
            if (line.contains("\"id\":" + id)) {
                if (line.contains("\"error\"")) {
                    throw new IOException("json-rpc error: " + line);
                }
                return line;
            }
        }
    }

    private String callTool(String name, String argsJson) throws IOException {
        String params = jsonObj("name", jsonStr(name), "arguments", argsJson);
        return request("tools/call", params);
    }

    // ---- Knowledge Object tools --------------------------------------------

    public String remember(String typeName, String propertiesJson) throws IOException {
        String args = jsonObj(
            "subject", jsonStr("sdk-user"),
            "type_name", jsonStr(typeName),
            "properties", propertiesJson
        );
        return callTool("remember", args);
    }

    public String get(String koid) throws IOException {
        String args = jsonObj("subject", jsonStr("sdk-user"), "koid", jsonStr(koid));
        return callTool("get", args);
    }

    public String findSimilar(String text, int k) throws IOException {
        String args = jsonObj(
            "subject", jsonStr("sdk-user"),
            "text", jsonStr(text),
            "k", jsonNum(k)
        );
        return callTool("find_similar", args);
    }

    public String aikoql(String query) throws IOException {
        String args = jsonObj("subject", jsonStr("sdk-user"), "query", jsonStr(query));
        return callTool("aikoql", args);
    }

    public String backup() throws IOException {
        return callTool("backup", "{}");
    }

    public String restore(String backup) throws IOException {
        String args = jsonObj("backup", jsonStr(backup));
        return callTool("restore", args);
    }

    public String metrics() throws IOException {
        return callTool("metrics", "{}");
    }

    // ---- Quick test --------------------------------------------------------

    public static void main(String[] args) throws Exception {
        if (args.length < 2) {
            System.out.println("Usage: AikoqlClient <host> <port>");
            return;
        }
        try (AikoqlClient db = new AikoqlClient(args[0], Integer.parseInt(args[1]))) {
            db.connect();
            db.initialize();
            System.out.println("metrics: " + db.metrics());
        }
    }
}
