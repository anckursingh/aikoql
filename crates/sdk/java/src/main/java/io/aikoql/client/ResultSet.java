package io.aikoql.client;

import java.util.Iterator;
import java.util.NoSuchElementException;

// The §4.3 stream result: a closeable chunk iterator over one query_stream.
// The response frame is the first chunk (for total_chunks == 1 there is no
// notify at all); notify frames with the matching stream_id follow until
// done or received == total_chunks. Pull-based under the session lock —
// close() cancels the pending read and releases the session, and the same
// teardown runs when the stream aborts mid-read. Mirrors the TS client's
// queryStream generator.
public final class ResultSet implements Iterator<Json.Value>, AutoCloseable {
    private final Connection conn;
    private final long id;
    private final Deadline dl;
    private String streamId;
    private long total;
    private long received;
    private Json.Value next;
    private boolean eof;
    private boolean finished;

    ResultSet(Connection conn, long id, Deadline dl) {
        this.conn = conn;
        this.id = id;
        this.dl = dl;
    }

    @Override
    public boolean hasNext() throws AikoqlException {
        // A buffered chunk (including a final head/notify that also set
        // eof) is yielded first; the stream ends on the next call.
        if (next != null) return true;
        if (eof) {
            finish();
            return false;
        }
        try {
            return pull();
        } catch (AikoqlException e) {
            finish();
            throw e;
        }
    }

    @Override
    public Json.Value next() {
        if (!hasNext()) throw new NoSuchElementException("stream exhausted");
        Json.Value v = next;
        next = null;
        return v;
    }

    /** Cancels the stream and releases the session (safe to call more than
     * once; iteration reaching the end releases it too). */
    @Override
    public void close() {
        finish();
    }

    private boolean pull() throws AikoqlException {
        for (;;) {
            String line = conn.transport.readLine(dl);
            if (line == null) {
                conn.latchClosed(); // §19: a dead stream read poisons the session
                throw AikoqlException.io("connection closed by the server");
            }
            Json.Value resp;
            try {
                resp = Json.parse(line);
            } catch (AikoqlException e) {
                continue;
            }
            Json.Value ridV = Json.dotGet(resp, "id");
            if (ridV instanceof Json.Num n && (long) n.v() == id) {
                Json.Value err = Json.dotGet(resp, "error");
                if (err != null) throw Connection.rpcError(err);
                Json.Value head = Json.dotGet(resp, "result");
                streamId = Json.jsonStr(Json.dotGet(head, "stream_id"));
                total = numVal(Json.dotGet(head, "total_chunks"));
                // The response frame IS the first chunk (it carries the
                // data; for total_chunks == 1 there is no notify at all) —
                // the Go, Python, Rust and TS SDKs yield it too.
                next = head == null ? Json.Null.NULL : head;
                received++;
                if (total > 0 && received >= total) eof = true;
                return true;
            }
            if (streamId == null || streamId.isEmpty()) continue; // push before the response frame
            StreamNotify sn = decodeStreamNotify(resp);
            if (sn == null || !sn.streamId().equals(streamId)) continue; // unrelated event
            Json.Value p = Json.dotGet(resp, "params");
            next = p == null ? Json.Null.NULL : p;
            received++;
            // The Go exit condition: done, or received == total_chunks.
            if (sn.done() || (total > 0 && received >= total)) {
                eof = true;
            }
            return true;
        }
    }

    private static long numVal(Json.Value v) {
        return v instanceof Json.Num n ? (long) n.v() : 0;
    }

    /** The pure §4.3 notify decode: a parsed frame's (stream_id, done)
     * pair, or null for anything that is not a notify frame. */
    static StreamNotify decodeStreamNotify(Json.Value resp) {
        Json.Value method = Json.dotGet(resp, "method");
        if (!(method instanceof Json.Str ms) || !ms.v().equals("notifications/notify")) {
            return null;
        }
        Json.Value p = Json.dotGet(resp, "params");
        Json.Value sid = Json.dotGet(p, "stream_id");
        if (!(sid instanceof Json.Str s)) return null;
        Json.Value done = Json.dotGet(p, "done");
        return new StreamNotify(s.v(), done instanceof Json.Bool b && b.v());
    }

    record StreamNotify(String streamId, boolean done) {}

    private void finish() {
        if (finished) return;
        finished = true;
        conn.transport.cancelPendingRead();
        conn.lock.unlock();
    }
}
