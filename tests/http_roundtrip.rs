use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

mod common;

struct Harness {
    child: Child,
    port: u16,
    /// Held for as long as the instance runs: the product allows one instance per
    /// machine, so no other test may start one meanwhile.
    _turn: common::Turn,
}

impl Harness {
    fn start(port: u16) -> Self {
        let turn = common::take_turn();
        let dir = std::env::temp_dir().join("glassine-plan-http");
        std::fs::create_dir_all(&dir).unwrap();
        // One file per port: cargo runs these test functions in parallel, and a
        // shared path lets one test overwrite another's port before its process
        // reads it.
        let config = dir.join(format!("config-{port}.toml"));
        std::fs::write(&config, format!("[server]\nport = {port}\n[window]\nsize = [320, 120]\n")).unwrap();

        let child = Command::new(env!("CARGO_BIN_EXE_glassine"))
            .arg("--config")
            .arg(&config)
            .spawn()
            .unwrap();
        let h = Harness { child, port, _turn: turn };
        h.wait_until_listening();
        h
    }

    fn wait_until_listening(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server never accepted a connection on port {}", self.port);
    }

    fn request(&self, raw: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn text_is_written_and_visible_in_status() {
    let h = Harness::start(47321);
    let body = "玻璃纸 glassine";
    let resp = h.request(&format!(
        "POST /text HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains(r#""mode":"text""#), "{resp}");

    let status = h.request("GET /status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(status.contains(r#""mode":"text""#), "{status}");
    assert!(status.contains(&format!(r#""text_bytes":{}"#, body.len())), "{status}");
}

#[test]
fn time_switches_the_mode_back() {
    let h = Harness::start(47322);
    h.request("POST /text HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\nhi");
    let resp = h.request("POST /time HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(resp.contains(r#""mode":"time""#), "{resp}");
}

#[test]
fn oversize_payload_is_rejected_and_does_not_change_the_content() {
    let h = Harness::start(47323);
    h.request("POST /text HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 3\r\n\r\nabc");
    let resp = h.request("POST /text HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 65537\r\n\r\n");
    assert!(resp.starts_with("HTTP/1.1 413"), "{resp}");

    let status = h.request("GET /status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(status.contains(r#""text_bytes":3"#), "content changed: {status}");
}

#[test]
fn a_disconnected_client_leaves_the_process_healthy() {
    let h = Harness::start(47324);
    h.request("POST /text HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 5\r\n\r\nhello");
    {
        // Connect and hang up mid-request.
        let mut s = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
        let _ = s.write_all(b"POST /text HTTP/1.1\r\nContent-Length: 100\r\n\r\npartial");
        drop(s);
    }
    std::thread::sleep(Duration::from_millis(500));
    let status = h.request("GET /status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(status.contains(r#""text_bytes":5"#), "{status}");
}

#[test]
fn a_body_larger_than_one_packet_is_read_completely() {
    let h = Harness::start(47326);
    // Comfortably past a single TCP segment, so the head and the body cannot
    // arrive in one read. Without the connection being put back into blocking
    // mode, the second read would fail immediately and the caller would be told
    // the request timed out.
    let body = "字".repeat(2048);
    let resp = h.request(&format!(
        "POST /text HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");

    let status = h.request("GET /status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(status.contains(&format!(r#""text_bytes":{}"#, body.len())), "{status}");
}

#[test]
fn a_silent_client_is_timed_out_with_408() {
    let h = Harness::start(47325);
    let mut s = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    // Announce a body, then never send it (spec §9.3: 408 after the 2 s timeout).
    s.write_all(b"POST /text HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 10\r\n\r\n").unwrap();

    let mut resp = String::new();
    let _ = s.read_to_string(&mut resp);
    assert!(resp.starts_with("HTTP/1.1 408"), "{resp:?}");
    assert!(resp.contains("timeout"), "{resp:?}");

    // The server must remain usable afterwards.
    let status = h.request("GET /status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
}
