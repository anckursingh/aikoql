package com.aikoql.client;

// The shared scripted-server helper for the wire and prepared tests — the
// port of the TS tests/helpers.ts scripted/respond/toolResult (the Rust
// suite's scripted server). Each server accepts one client connection and,
// per incoming line, hands the parsed request to `script` and writes back
// each returned frame after its delay (an empty frame list means a hung
// server, exactly like the Rust helper).

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;

final class Scripted {
    interface Script {
        List<Frame> run(Json.Obj req);
    }

    record Frame(String json, long delayMs) {}

    private Scripted() {}

    static ServerSocket scripted(Script script) throws IOException {
        ServerSocket ss = new ServerSocket(0, 50, InetAddress.getByName("127.0.0.1"));
        Thread t = new Thread(() -> {
            try (Socket sock = ss.accept();
                    BufferedReader in = new BufferedReader(
                            new InputStreamReader(sock.getInputStream(), StandardCharsets.UTF_8));
                    OutputStream out = sock.getOutputStream()) {
                String line;
                while ((line = in.readLine()) != null) {
                    Json.Value req;
                    try {
                        req = Json.parse(line);
                    } catch (AikoqlException e) {
                        continue;
                    }
                    if (!(req instanceof Json.Obj o)) continue;
                    for (Frame f : script.run(o)) {
                        if (f.delayMs() > 0) Thread.sleep(f.delayMs());
                        out.write((f.json() + "\n").getBytes(StandardCharsets.UTF_8));
                        out.flush();
                    }
                }
            } catch (IOException | InterruptedException ignored) {
            }
        }, "scripted-server");
        t.setDaemon(true);
        t.start();
        return ss;
    }

    /** The initialize answer with a pinned server version. */
    static String respond(long id, String version) {
        return "{\"id\":" + id + ",\"result\":{\"serverInfo\":{\"version\":\"" + version + "\"}}}";
    }

    /** A tools/call answer whose text carries {ok:true,data:<dataJson>}. */
    static String toolResult(long id, String dataJson) {
        String text =
                Json.stringify(Json.from(Map.of("ok", true, "data", Json.parse(dataJson))));
        return "{\"id\":" + id + ",\"result\":{\"content\":[{\"text\":"
                + Json.stringify(new Json.Str(text)) + "}],\"isError\":false}}";
    }

    /** A tools/call answer whose text carries the given error envelope. */
    static String toolError(long id, String code, String message) {
        String text = "{\"ok\":false,\"error\":{\"code\":\"" + code
                + "\",\"message\":\"" + message + "\",\"retryable\":false,\"suggestion\":\"\"}}";
        return "{\"id\":" + id + ",\"result\":{\"content\":[{\"text\":"
                + Json.stringify(new Json.Str(text)) + "}],\"isError\":true}}";
    }

    static long num(Json.Value v) {
        return (long) ((Json.Num) v).v();
    }
}
