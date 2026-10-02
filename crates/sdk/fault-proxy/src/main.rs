//! D-16: the §18 fault proxy — one instance, one fault mode, one client
//! connection at a time (the matrix test spawns an instance per fault).
//! Sits between an SDK client and a real server and mangles the wire in
//! exactly one of the thirteen §18 ways, so every SDK can run the same
//! fault matrix against the same misbehaving server.
//!
//! Wires (--wire, default native):
//!   native  the §6 frames (header + payload + crc)
//!   line    newline-delimited JSON-RPC (the MCP wire the four SDKs speak)
//! A message is one raw byte blob either way — only the read/write
//! primitives and three structural modes (corrupt/oversized/truncate)
//! care which wire is in use.
//!
//! Modes (frame accounting starts at the client's first message):
//!   drop-request --n N        drop the Nth client→server message
//!   drop-response --n N       drop the Nth server→client message
//!   delay-response --from N --delay-ms D
//!                             delay responses N+ by D ms
//!   duplicate-response --n N  forward response N twice
//!   reorder-response --n N    hold response N until the next request
//!                             passes, then send it (out of order)
//!   truncate-response --n N --bytes K
//!                             cut the last K bytes off response N (on the
//!                             line wire the newline is re-appended — a
//!                             cut newline would glue the next response on)
//!   corrupt-response --n N    flip one byte (checksum fails / noise)
//!   inject-notification --after N
//!                             after response N, push a message without
//!                             the response marker (a PING frame on the
//!                             native wire, an id-less JSON line on the
//!                             line wire — the closest analog either has)
//!   inject-stale-response --after N
//!                             replay response #1 after response N
//!   close-after --n N         close the socket after response N
//!   half-close-after --n N    shutdown(Write) after response N — the
//!                             client can still write but reads EOF
//!   slow-server --from N --bytes K --delay-ms D
//!                             drip responses N+ at K bytes per D ms
//!   oversized-response --n N --claim B
//!                             rewrite response N to claim B bytes, then
//!                             close — the client must reject from the
//!                             1 MiB cap before buffering B (§19)
//!
//! ponytail: two blocking read/forward threads and one shared Mutex —
//! this is a test tool, throughput is not a concern.

use aikoql_native as nat;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

/// The proxy-side sanity bound on one line (the SDKs cap at 1 MiB).
const LINE_MAX: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    DropRequest,
    DropResponse,
    DelayResponse,
    DuplicateResponse,
    ReorderResponse,
    TruncateResponse,
    CorruptResponse,
    InjectNotification,
    InjectStaleResponse,
    CloseAfter,
    HalfCloseAfter,
    SlowServer,
    OversizedResponse,
}

#[derive(Clone)]
struct Mode {
    kind: Kind,
    n: usize,
    from: usize,
    bytes: usize,
    delay_ms: u64,
    claim: usize,
    line: bool,
}

fn parse_args() -> (String, String, Mode) {
    let mut listen = String::new();
    let mut target = String::new();
    let mut kind = None;
    let mut m = Mode {
        kind: Kind::DropRequest,
        n: 0,
        from: 0,
        bytes: 0,
        delay_ms: 0,
        claim: 0,
        line: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().expect("missing value");
        match a.as_str() {
            "--listen" => listen = val(),
            "--wire" => m.line = val() == "line",
            "--target" => target = val(),
            "--mode" => {
                kind = Some(match val().as_str() {
                    "drop-request" => Kind::DropRequest,
                    "drop-response" => Kind::DropResponse,
                    "delay-response" => Kind::DelayResponse,
                    "duplicate-response" => Kind::DuplicateResponse,
                    "reorder-response" => Kind::ReorderResponse,
                    "truncate-response" => Kind::TruncateResponse,
                    "corrupt-response" => Kind::CorruptResponse,
                    "inject-notification" => Kind::InjectNotification,
                    "inject-stale-response" => Kind::InjectStaleResponse,
                    "close-after" => Kind::CloseAfter,
                    "half-close-after" => Kind::HalfCloseAfter,
                    "slow-server" => Kind::SlowServer,
                    "oversized-response" => Kind::OversizedResponse,
                    other => panic!("unknown fault mode {other}"),
                });
            }
            "--n" => m.n = val().parse().expect("--n"),
            // the inject modes phrase the same parameter as "after"
            "--after" => m.n = val().parse().expect("--after"),
            "--from" => m.from = val().parse().expect("--from"),
            "--bytes" => m.bytes = val().parse().expect("--bytes"),
            "--delay-ms" => m.delay_ms = val().parse().expect("--delay-ms"),
            "--claim" => m.claim = val().parse().expect("--claim"),
            other => panic!("unknown flag {other}"),
        }
    }
    m.kind = kind.expect("--mode is required");
    (listen, target, m)
}

/// One wire message as a raw byte blob — the proxy does not validate what
/// it forwards (it mangles a healthy stream, it does not police it).
fn read_msg(r: &mut impl BufRead, m: &Mode) -> std::io::Result<Vec<u8>> {
    if m.line {
        let mut buf = Vec::new();
        if r.read_until(b'\n', &mut buf)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        if buf.len() > LINE_MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "over-cap line",
            ));
        }
        Ok(buf)
    } else {
        let mut buf = vec![0u8; nat::HEADER_LEN];
        r.read_exact(&mut buf)?;
        let len = u32::from_be_bytes(buf[18..22].try_into().expect("4 bytes")) as usize;
        assert!(len <= nat::MAX_PAYLOAD, "server sent an over-cap frame");
        buf.resize(nat::HEADER_LEN + len + 4, 0);
        r.read_exact(&mut buf[nat::HEADER_LEN..])?;
        Ok(buf)
    }
}

fn write_msg(w: &mut TcpStream, msg: &[u8]) -> std::io::Result<()> {
    w.write_all(msg)?;
    w.flush()
}

struct Shared {
    /// The response held back by reorder-response.
    held: Mutex<Option<Vec<u8>>>,
    /// Set by the client→server pump when the next request passes while a
    /// response is held; wakes the holding thread.
    release_flag: Mutex<bool>,
    release_cv: Condvar,
    /// Response #1, replayed by inject-stale-response.
    recorded: Mutex<Option<Vec<u8>>>,
    /// The connection is done: both pumps wind down.
    down: AtomicBool,
}

fn handle(client: TcpStream, server: TcpStream, m: &Mode) {
    let client_w = client.try_clone().expect("client write handle");
    let server_w = server.try_clone().expect("server write handle");
    let s = Arc::new(Shared {
        held: Mutex::new(None),
        release_flag: Mutex::new(false),
        release_cv: Condvar::new(),
        recorded: Mutex::new(None),
        down: AtomicBool::new(false),
    });

    // client → server
    {
        let s = s.clone();
        let m = m.clone();
        thread::spawn(move || {
            let (mut cr, mut sw) = (BufReader::new(client), server_w);
            let mut idx = 0usize;
            loop {
                if s.down.load(Ordering::Relaxed) {
                    break;
                }
                match read_msg(&mut cr, &m) {
                    Ok(msg) => {
                        idx += 1;
                        if m.kind == Kind::DropRequest && idx == m.n {
                            continue;
                        }
                        // reorder-release: a request passing through while
                        // a response is held back frees it first.
                        if s.held.lock().unwrap().is_some() {
                            *s.release_flag.lock().unwrap() = true;
                            s.release_cv.notify_all();
                        }
                        if write_msg(&mut sw, &msg).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        s.down.store(true, Ordering::Relaxed);
                        let _ = sw.shutdown(Shutdown::Both);
                        break;
                    }
                }
            }
        });
    }

    // server → client
    {
        let s = s.clone();
        let m = m.clone();
        thread::spawn(move || {
            let (mut sr, mut cw) = (BufReader::new(server), client_w);
            let mut idx = 0usize;
            let mut half = false;
            loop {
                if s.down.load(Ordering::Relaxed) {
                    break;
                }
                let msg = match read_msg(&mut sr, &m) {
                    Ok(f) => f,
                    Err(_) => {
                        s.down.store(true, Ordering::Relaxed);
                        let _ = cw.shutdown(Shutdown::Both);
                        break;
                    }
                };
                idx += 1;
                if idx == 1 {
                    *s.recorded.lock().unwrap() = Some(msg.clone());
                }
                if half {
                    continue; // half-closed: the client reads EOF, we discard
                }
                match m.kind {
                    Kind::DropResponse if idx == m.n => {}
                    Kind::DelayResponse if idx >= m.from => {
                        thread::sleep(Duration::from_millis(m.delay_ms));
                        let _ = write_msg(&mut cw, &msg);
                    }
                    Kind::DuplicateResponse if idx == m.n => {
                        let _ = write_msg(&mut cw, &msg);
                        let _ = write_msg(&mut cw, &msg);
                    }
                    Kind::ReorderResponse if idx == m.n => {
                        *s.held.lock().unwrap() = Some(msg);
                        let mut g = s.release_flag.lock().unwrap();
                        while !*g {
                            g = s.release_cv.wait(g).unwrap();
                        }
                        let f = s.held.lock().unwrap().take().unwrap();
                        let _ = write_msg(&mut cw, &f);
                    }
                    Kind::TruncateResponse if idx == m.n => {
                        let mut buf = msg.clone();
                        if m.line {
                            // keep the newline: cutting it would glue the
                            // next response onto the truncated line
                            if buf.last() == Some(&b'\n') {
                                buf.pop();
                            }
                        }
                        let cut = m.bytes.min(buf.len());
                        let _ = cw.write_all(&buf[..buf.len() - cut]);
                        if m.line {
                            let _ = cw.write_all(b"\n");
                        }
                        let _ = cw.flush();
                    }
                    Kind::CorruptResponse if idx == m.n => {
                        let mut buf = msg.clone();
                        // the line's first byte becomes noise; the frame's
                        // payload flips while the checksum covers the
                        // original — the client rejects it either way
                        let off = if m.line { 0 } else { nat::HEADER_LEN };
                        buf[off] ^= 0xFF;
                        let _ = write_msg(&mut cw, &buf);
                    }
                    Kind::InjectNotification => {
                        let _ = write_msg(&mut cw, &msg);
                        if idx == m.n {
                            if m.line {
                                // an id-less JSON line is never a response
                                let _ =
                                    write_msg(&mut cw, b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n");
                            } else {
                                // a well-formed frame without the response
                                // flag (no notification class on the §6 wire)
                                let payload = b"{}";
                                let hdr = nat::header_bytes(0, 999, nat::PING, payload.len() as u32);
                                let mut buf = hdr.to_vec();
                                buf.extend_from_slice(payload);
                                let crc = nat::crc32(&buf).to_le_bytes();
                                buf.extend_from_slice(&crc);
                                let _ = write_msg(&mut cw, &buf);
                            }
                        }
                    }
                    Kind::InjectStaleResponse => {
                        let _ = write_msg(&mut cw, &msg);
                        if idx == m.n {
                            if let Some(f) = s.recorded.lock().unwrap().clone() {
                                let _ = write_msg(&mut cw, &f);
                            }
                        }
                    }
                    Kind::CloseAfter => {
                        let _ = write_msg(&mut cw, &msg);
                        if idx == m.n {
                            s.down.store(true, Ordering::Relaxed);
                            let _ = cw.shutdown(Shutdown::Both);
                            break;
                        }
                    }
                    Kind::HalfCloseAfter => {
                        let _ = write_msg(&mut cw, &msg);
                        if idx == m.n {
                            // half-close: the client can still write, but
                            // its reads get EOF — keep draining the server.
                            let _ = cw.shutdown(Shutdown::Write);
                            half = true;
                        }
                    }
                    Kind::SlowServer if idx >= m.from => {
                        for chunk in msg.chunks(m.bytes) {
                            let _ = cw.write_all(chunk);
                            let _ = cw.flush();
                            thread::sleep(Duration::from_millis(m.delay_ms));
                        }
                    }
                    Kind::OversizedResponse if idx == m.n => {
                        // Claim B bytes, deliver a fraction, close: the
                        // client must reject from its 1 MiB cap before
                        // buffering B (§19). The native client rejects from
                        // the patched header alone; the line clients trip
                        // mid-accumulation on the unterminated junk.
                        if m.line {
                            // ponytail: drip 1 MiB slices; a client that
                            // stops reading stalls one slice, not the pump
                            // forever — the test kills us anyway.
                            let junk = vec![b'x'; m.claim.min(1 << 20)];
                            let mut remaining = m.claim;
                            while remaining > 0 {
                                let n = junk.len().min(remaining);
                                if cw.write_all(&junk[..n]).is_err() {
                                    break;
                                }
                                remaining -= n;
                            }
                            let _ = cw.flush();
                        } else {
                            let mut buf = msg.clone();
                            buf[18..22].copy_from_slice(&(m.claim as u32).to_be_bytes());
                            let _ = write_msg(&mut cw, &buf);
                        }
                        s.down.store(true, Ordering::Relaxed);
                        let _ = cw.shutdown(Shutdown::Both);
                        break;
                    }
                    _ => {
                        let _ = write_msg(&mut cw, &msg);
                    }
                }
            }
        });
    }
    // The c2s/s2c threads wind down when the connection does; the accept
    // loop keeps serving the next connection until the test kills us.
}

fn main() {
    let (listen, target, mode) = parse_args();
    let listener = TcpListener::bind(&listen).expect("bind --listen");
    loop {
        let (client, _) = listener.accept().expect("accept");
        let server = match TcpStream::connect(&target) {
            Ok(s) => s,
            Err(_) => {
                let _ = client.shutdown(Shutdown::Both);
                continue;
            }
        };
        let m = mode.clone();
        thread::spawn(move || handle(client, server, &m));
    }
}
