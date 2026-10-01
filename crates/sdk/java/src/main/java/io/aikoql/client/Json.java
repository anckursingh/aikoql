package io.aikoql.client;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

// Minimal JSON value + parser + emitter — the JDK ships no JSON and the SDK
// must stay zero-dependency. The wire layer parses every frame with it and
// the canonical API hands back these values.
final class Json {
    private Json() {}

    // — Values —

    sealed interface Value {}
    enum Null implements Value { NULL }
    record Bool(boolean v) implements Value {}
    record Num(double v) implements Value {}
    record Str(String v) implements Value {}
    static final class Arr implements Value {
        final List<Value> items = new ArrayList<>();
    }
    static final class Obj implements Value {
        final Map<String, Value> fields = new LinkedHashMap<>();
    }

    // — Building —

    /** Converts Map/List/String/Number/Boolean/null (or a Value, as-is)
     * into a Value for serialization. */
    static Value from(Object o) {
        if (o == null) return Null.NULL;
        if (o instanceof Value v) return v;
        if (o instanceof String s) return new Str(s);
        if (o instanceof Boolean b) return new Bool(b);
        if (o instanceof Number n) return new Num(n.doubleValue());
        if (o instanceof Map<?, ?> m) {
            Obj obj = new Obj();
            for (Map.Entry<?, ?> e : m.entrySet()) {
                obj.fields.put(String.valueOf(e.getKey()), from(e.getValue()));
            }
            return obj;
        }
        if (o instanceof List<?> l) {
            Arr a = new Arr();
            for (Object item : l) a.items.add(from(item));
            return a;
        }
        throw AikoqlException.invalidArgument("cannot serialize " + o.getClass());
    }

    /** The dotted-path access the wire layer and the conformance checks
     * share: object keys and numeric array indexes; null on any miss. */
    static Value dotGet(Value root, String path) {
        Value cur = root;
        for (String part : path.split("\\.")) {
            if (cur instanceof Obj o) {
                cur = o.fields.get(part);
            } else if (cur instanceof Arr a) {
                int idx;
                try {
                    idx = Integer.parseInt(part);
                } catch (NumberFormatException e) {
                    return null;
                }
                if (idx < 0 || idx >= a.items.size()) return null;
                cur = a.items.get(idx);
            } else {
                return null;
            }
            if (cur == null) return null;
        }
        return cur;
    }

    /** String form of a Str or integral Num (error codes arrive as both);
     * "" otherwise. */
    static String jsonStr(Value v) {
        if (v instanceof Str s) return s.v();
        if (v instanceof Num n) {
            double d = n.v();
            return d == (long) d ? String.valueOf((long) d) : String.valueOf(d);
        }
        return "";
    }

    /** Numeric-aware equality: 3 and 3.0 compare equal, like Python's ==
     * and unlike Java's Double.equals — the vectors were frozen against
     * both the float-only (Go) and int/float (Python) decode paths. */
    static boolean jsonEq(Value a, Value b) {
        if (a instanceof Num x && b instanceof Num y) {
            double xv = x.v();
            double yv = y.v();
            if (xv == (long) xv && yv == (long) yv) return (long) xv == (long) yv;
            return xv == yv;
        }
        if (a instanceof Arr x && b instanceof Arr y) {
            if (x.items.size() != y.items.size()) return false;
            for (int i = 0; i < x.items.size(); i++) {
                if (!jsonEq(x.items.get(i), y.items.get(i))) return false;
            }
            return true;
        }
        if (a instanceof Obj x && b instanceof Obj y) {
            if (x.fields.size() != y.fields.size()) return false;
            for (Map.Entry<String, Value> e : x.fields.entrySet()) {
                Value w = y.fields.get(e.getKey());
                if (w == null || !jsonEq(e.getValue(), w)) return false;
            }
            return true;
        }
        return a.equals(b);
    }

    // — Parsing —

    static Value parse(String text) throws AikoqlException {
        Parser p = new Parser(text);
        Value v = p.value();
        p.skipWs();
        if (!p.eof()) throw err("trailing content", p.pos);
        return v;
    }

    private static AikoqlException err(String what, int pos) {
        return AikoqlException.json(what + " at " + pos);
    }

    private static final class Parser {
        final String s;
        int pos;

        Parser(String s) {
            this.s = s;
        }

        boolean eof() {
            return pos >= s.length();
        }

        void skipWs() {
            while (!eof()) {
                char c = s.charAt(pos);
                if (c == ' ' || c == '\t' || c == '\n' || c == '\r') pos++;
                else break;
            }
        }

        Value value() {
            skipWs();
            if (eof()) throw err("unexpected end", pos);
            return switch (s.charAt(pos)) {
                case '{' -> obj();
                case '[' -> arr();
                case '"' -> new Str(str());
                case 't' -> { lit("true"); yield new Bool(true); }
                case 'f' -> { lit("false"); yield new Bool(false); }
                case 'n' -> { lit("null"); yield Null.NULL; }
                default -> num();
            };
        }

        void lit(String word) {
            if (!s.startsWith(word, pos)) throw err("bad literal", pos);
            pos += word.length();
        }

        Obj obj() {
            Obj o = new Obj();
            pos++; // {
            skipWs();
            if (!eof() && s.charAt(pos) == '}') {
                pos++;
                return o;
            }
            for (;;) {
                skipWs();
                if (eof() || s.charAt(pos) != '"') throw err("expected key", pos);
                String key = str();
                skipWs();
                if (eof() || s.charAt(pos) != ':') throw err("expected ':'", pos);
                pos++;
                o.fields.put(key, value());
                skipWs();
                if (eof()) throw err("unterminated object", pos);
                char c = s.charAt(pos);
                if (c == '}') {
                    pos++;
                    return o;
                }
                if (c != ',') throw err("expected ',' or '}'", pos);
                pos++;
            }
        }

        Arr arr() {
            Arr a = new Arr();
            pos++; // [
            skipWs();
            if (!eof() && s.charAt(pos) == ']') {
                pos++;
                return a;
            }
            for (;;) {
                a.items.add(value());
                skipWs();
                if (eof()) throw err("unterminated array", pos);
                char c = s.charAt(pos);
                if (c == ']') {
                    pos++;
                    return a;
                }
                if (c != ',') throw err("expected ',' or ']'", pos);
                pos++;
            }
        }

        String str() {
            pos++; // opening quote
            StringBuilder sb = new StringBuilder();
            while (!eof()) {
                char c = s.charAt(pos++);
                if (c == '"') return sb.toString();
                if (c != '\\') {
                    sb.append(c);
                    continue;
                }
                if (eof()) throw err("bad escape", pos);
                char e = s.charAt(pos++);
                switch (e) {
                    case '"' -> sb.append('"');
                    case '\\' -> sb.append('\\');
                    case '/' -> sb.append('/');
                    case 'b' -> sb.append('\b');
                    case 'f' -> sb.append('\f');
                    case 'n' -> sb.append('\n');
                    case 'r' -> sb.append('\r');
                    case 't' -> sb.append('\t');
                    case 'u' -> {
                        if (pos + 4 > s.length()) throw err("bad \\u", pos);
                        String hex = s.substring(pos, pos + 4);
                        pos += 4;
                        try {
                            sb.append((char) Integer.parseInt(hex, 16));
                        } catch (NumberFormatException ex) {
                            throw err("bad \\u", pos - 4);
                        }
                        // Vectors are ASCII; a lone surrogate passes
                        // through as-is (chars are UTF-16 anyway).
                    }
                    default -> throw err("bad escape", pos);
                }
            }
            throw err("unterminated string", pos);
        }

        Num num() {
            int start = pos;
            if (!eof() && s.charAt(pos) == '-') pos++;
            while (!eof()) {
                char c = s.charAt(pos);
                if ((c >= '0' && c <= '9') || c == '.' || c == 'e' || c == 'E'
                        || c == '+' || c == '-') pos++;
                else break;
            }
            if (pos == start || (pos == start + 1 && s.charAt(start) == '-')) {
                throw err("bad number", start);
            }
            try {
                return new Num(Double.parseDouble(s.substring(start, pos)));
            } catch (NumberFormatException e) {
                throw err("bad number", start);
            }
        }
    }

    // — Emitting —

    static String stringify(Value v) {
        StringBuilder sb = new StringBuilder();
        write(sb, v);
        return sb.toString();
    }

    private static void write(StringBuilder sb, Value v) {
        if (v instanceof Null) {
            sb.append("null");
            return;
        }
        if (v instanceof Bool b) {
            sb.append(b.v());
            return;
        }
        if (v instanceof Num n) {
            double d = n.v();
            if (d == Math.rint(d) && !Double.isInfinite(d) && Math.abs(d) < 1e15) {
                sb.append((long) d);
            } else {
                sb.append(d);
            }
            return;
        }
        if (v instanceof Str s) {
            writeString(sb, s.v());
            return;
        }
        if (v instanceof Arr a) {
            sb.append('[');
            boolean first = true;
            for (Value item : a.items) {
                if (!first) sb.append(',');
                first = false;
                write(sb, item);
            }
            sb.append(']');
            return;
        }
        Obj o = (Obj) v;
        sb.append('{');
        boolean first = true;
        for (Map.Entry<String, Value> e : o.fields.entrySet()) {
            if (!first) sb.append(',');
            first = false;
            writeString(sb, e.getKey());
            sb.append(':');
            write(sb, e.getValue());
        }
        sb.append('}');
    }

    private static void writeString(StringBuilder sb, String s) {
        sb.append('"');
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"' -> sb.append("\\\"");
                case '\\' -> sb.append("\\\\");
                case '\n' -> sb.append("\\n");
                case '\r' -> sb.append("\\r");
                case '\t' -> sb.append("\\t");
                case '\b' -> sb.append("\\b");
                case '\f' -> sb.append("\\f");
                default -> {
                    if (c < 0x20) sb.append(String.format("\\u%04x", (int) c));
                    else sb.append(c);
                }
            }
        }
        sb.append('"');
    }
}
