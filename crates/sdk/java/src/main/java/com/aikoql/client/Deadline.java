package io.aikoql.client;

// The AbortSignal stand-in: the abort path races the response read, and the
// transport registers its wake-up here (register-then-recheck is atomic
// under the deadline's own monitor). Mirrors the TS AbortController's role
// in withDeadline.
public final class Deadline {
    private boolean aborted;
    private Runnable onAbort;

    public synchronized void abort() {
        aborted = true;
        if (onAbort != null) onAbort.run();
    }

    public synchronized boolean isAborted() {
        return aborted;
    }

    synchronized void onAbort(Runnable r) {
        if (aborted) r.run();
        else onAbort = r;
    }
}
