//! D-16: the §18 fault proxy — one instance, one fault mode, one client
//! connection at a time (the matrix test spawns an instance per fault).
//! Sits between an SDK client and a real server and mangles the native
//! wire in exactly one of the thirteen §18 ways, so every SDK can run
//! the same fault matrix against the same misbehaving server.
//!
//! Modes (frame accounting starts at the client's HELLO):
//!   drop-request --n N        drop the Nth client→server frame
//!   drop-response --n N       drop the Nth server→client frame
//!   delay-response --from N --delay-ms D
//!                             delay responses N+ by D ms
//!   duplicate-response --n N  forward response N twice
//!   reorder-response --n N    hold response N until the next request
//!                             passes, then send it (out of order)
//!   truncate-response --n N --bytes K
//!                             cut the last K bytes off response N
//!   corrupt-response --n N    flip one payload byte (checksum fails)
//!   inject-notification --after N
//!                             after response N, push a well-formed frame
//!                             without the response flag (the §6 wire has
//!                             no notification class — the closest analog)
//!   inject-stale-response --after N
//!                             replay response #1 after response N
//!   close-after --n N         close the socket after response N
//!   half-close-after --n N    shutdown(Write) after response N — the
//!                             client can still write but reads EOF
//!   slow-server --from N --bytes K --delay-ms D
//!                             drip responses N+ at K bytes per D ms
//!   oversized-response --n N --claim B
//!                             rewrite response N's header to claim B
//!                             bytes, send junk, close — the client must
//!                             reject from the header before allocating
//!
//! ponytail: two blocking read/forward threads and one shared Mutex —
//! this is a test tool, throughput is not a concern.

use aikoql_native as nat;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

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
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().expect("missing value");
        match a.as_str() {
            "--listen" => listen = val(),
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

/// The frame's three parts, read raw — the proxy does not validate what
/// it forwards (it mangles a healthy stream, it does not police it).
fn read_frame(r: &mut impl Read) -> std::io::Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let mut h = vec![0u8; nat::HEADER_LEN];
    r.read_exact(&mut h)?;
    let len = u32::from_be_bytes(h[18..22].try_into().expect("4 bytes")) as usize;
    assert!(len <= nat::MAX_PAYLOAD, "server sent an over-cap frame");
    let mut p = vec![0u8; len];
    r.read_exact(&mut p)?;
    let mut c = vec![0u8; 4];
    r.read_exact(&mut c)?;
    Ok((h, p, c))
}

fn write_frame(w: &mut impl Write, h: &[u8], p: &[u8], c: &[u8]) -> std::io::Result<()> {
    w.write_all(h)?;
    w.write_all(p)?;
    w.write_all(c)?;
    w.flush()
}

struct Shared {
    /// The response held back by reorder-response.
    held: Mutex<Option<(Vec<u8>, Vec<u8>, Vec<u8>)>>,
    /// Set by the client→server pump when the next request passes while a
    /// response is held; wakes the holding thread.
    release_flag: Mutex<bool>,
    release_cv: Condvar,
    /// Response #1, replayed by inject-stale-response.
    recorded: Mutex<Option<(Vec<u8>, Vec<u8>, Vec<u8>)>>,
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
            let (mut cr, mut sw) = (client, server_w);
            let mut idx = 0usize;
            loop {
                if s.down.load(Ordering::Relaxed) {
                    break;
                }
                match read_frame(&mut cr) {
                    Ok(frame) => {
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
                        if write_frame(&mut sw, &frame.0, &frame.1, &frame.2).is_err() {
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
            let (mut sr, mut cw) = (server, client_w);
            let mut idx = 0usize;
            let mut half = false;
            loop {
                if s.down.load(Ordering::Relaxed) {
                    break;
                }
                let frame = match read_frame(&mut sr) {
                    Ok(f) => f,
                    Err(_) => {
                        s.down.store(true, Ordering::Relaxed);
                        let _ = cw.shutdown(Shutdown::Both);
                        break;
                    }
                };
                idx += 1;
                if idx == 1 {
                    *s.recorded.lock().unwrap() = Some(frame.clone());
                }
                if half {
                    continue; // half-closed: the client reads EOF, we discard
                }
                let write =
                    |cw: &mut TcpStream, h: &[u8], p: &[u8], c: &[u8]| write_frame(cw, h, p, c);
                match m.kind {
                    Kind::DropResponse if idx == m.n => {}
                    Kind::DelayResponse if idx >= m.from => {
                        thread::sleep(Duration::from_millis(m.delay_ms));
                        let _ = write(&mut cw, &frame.0, &frame.1, &frame.2);
                    }
                    Kind::DuplicateResponse if idx == m.n => {
                        let _ = write(&mut cw, &frame.0, &frame.1, &frame.2);
                        let _ = write(&mut cw, &frame.0, &frame.1, &frame.2);
                    }
                    Kind::ReorderResponse if idx == m.n => {
                        *s.held.lock().unwrap() = Some(frame);
                        let mut g = s.release_flag.lock().unwrap();
                        while !*g {
                            g = s.release_cv.wait(g).unwrap();
                        }
                        let f = s.held.lock().unwrap().take().unwrap();
                        let _ = write(&mut cw, &f.0, &f.1, &f.2);
                    }
                    Kind::TruncateResponse if idx == m.n => {
                        let mut buf = Vec::new();
                        buf.extend_from_slice(&frame.0);
                        buf.extend_from_slice(&frame.1);
                        buf.extend_from_slice(&frame.2);
                        let cut = m.bytes.min(buf.len());
                        let _ = cw.write_all(&buf[..buf.len() - cut]);
                        let _ = cw.flush();
                    }
                    Kind::CorruptResponse if idx == m.n => {
                        let mut p = frame.1.clone();
                        p[0] ^= 0xFF; // the wire checksum still covers the original
                        let _ = write(&mut cw, &frame.0, &p, &frame.2);
                    }
                    Kind::InjectNotification => {
                        let _ = write(&mut cw, &frame.0, &frame.1, &frame.2);
                        if idx == m.n {
                            // a well-formed frame without the response flag
                            // (no notification class exists on the §6 wire)
                            let payload = b"{}";
                            let hdr = nat::header_bytes(0, 999, nat::PING, payload.len() as u32);
                            let mut buf = hdr.to_vec();
                            buf.extend_from_slice(payload);
                            let crc = nat::crc32(&buf).to_le_bytes();
                            let _ = cw.write_all(&buf);
                            let _ = cw.write_all(&crc);
                            let _ = cw.flush();
                        }
                    }
                    Kind::InjectStaleResponse => {
                        let _ = write(&mut cw, &frame.0, &frame.1, &frame.2);
                        if idx == m.n {
                            if let Some(f) = s.recorded.lock().unwrap().clone() {
                                let _ = write(&mut cw, &f.0, &f.1, &f.2);
                            }
                        }
                    }
                    Kind::CloseAfter => {
                        let _ = write(&mut cw, &frame.0, &frame.1, &frame.2);
                        if idx == m.n {
                            s.down.store(true, Ordering::Relaxed);
                            let _ = cw.shutdown(Shutdown::Both);
                            break;
                        }
                    }
                    Kind::HalfCloseAfter => {
                        let _ = write(&mut cw, &frame.0, &frame.1, &frame.2);
                        if idx == m.n {
                            // half-close: the client can still write, but
                            // its reads get EOF — keep draining the server.
                            let _ = cw.shutdown(Shutdown::Write);
                            half = true;
                        }
                    }
                    Kind::SlowServer if idx >= m.from => {
                        let mut buf = Vec::new();
                        buf.extend_from_slice(&frame.0);
                        buf.extend_from_slice(&frame.1);
                        buf.extend_from_slice(&frame.2);
                        for chunk in buf.chunks(m.bytes) {
                            let _ = cw.write_all(chunk);
                            let _ = cw.flush();
                            thread::sleep(Duration::from_millis(m.delay_ms));
                        }
                    }
                    Kind::OversizedResponse if idx == m.n => {
                        // claim B bytes, deliver a fraction, close: the
                        // client must reject from the header alone (§19).
                        let mut h = frame.0.clone();
                        h[18..22].copy_from_slice(&(m.claim as u32).to_be_bytes());
                        let _ = cw.write_all(&h);
                        let _ = cw.write_all(&[0u8; 64]);
                        let _ = cw.flush();
                        s.down.store(true, Ordering::Relaxed);
                        let _ = cw.shutdown(Shutdown::Both);
                        break;
                    }
                    _ => {
                        let _ = write(&mut cw, &frame.0, &frame.1, &frame.2);
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
