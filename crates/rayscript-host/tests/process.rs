//! The real binary, on real pipes.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Proc {
    fn start() -> Proc {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rayscript-host")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().expect("the host starts");
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for l in out.lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Proc { child, stdin, lines }
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// The next line that contains `needle`, within two seconds; the lines before it are dropped.
    fn expect(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) if l.contains(needle) => return l,
                Ok(_) => {}
                Err(_) => panic!("no line with {needle:?} came within two seconds"),
            }
        }
    }

    fn wait_exit(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.success();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = self.child.kill();
        panic!("the host did not exit");
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn the_host_serves_a_view_and_its_events_over_pipes() {
    let mut p = Proc::start();
    assert_eq!(p.expect("ready"), "{\"t\":\"ready\",\"protocol\":1}");
    p.send(r#"{"t":"run","source":"var n = signal(0); mount(() -> <button onClick={() -> n.value++}>{n}</button>, 'main');"}"#);
    let ops = p.expect("\"ops\"");
    assert!(ops.contains("[\"create\",1,\"#root\"]") && ops.contains("[\"create\",2,\"button\"]") && ops.contains("[\"listen\",2,\"click\"]"), "{ops}");
    p.send(r#"{"t":"event","node":2,"event":"click"}"#);
    let ops = p.expect("setText");
    assert_eq!(ops, "{\"t\":\"ops\",\"ops\":[[\"setText\",4,\"1\"]]}");
    p.send(r#"{"t":"shutdown"}"#);
    assert!(p.wait_exit());
}

#[test]
fn closing_the_input_ends_the_host() {
    let mut p = Proc::start();
    p.expect("ready");
    p.stdin = None;
    assert!(p.wait_exit());
}

#[test]
fn a_timer_fires_with_no_input_to_wake_the_host() {
    let mut p = Proc::start();
    p.expect("ready");
    p.send(r#"{"t":"run","source":"await sleep(60); trace('woke');"}"#);
    let started = Instant::now();
    p.expect("woke");
    assert!(started.elapsed() >= Duration::from_millis(50));
}

#[test]
fn a_script_that_never_ends_does_not_stop_the_host_from_listening() {
    let mut p = Proc::start();
    p.expect("ready");
    p.send(r#"{"t":"run","source":"while (true) { }"}"#);
    std::thread::sleep(Duration::from_millis(50));
    p.send(r#"{"t":"describe"}"#);
    let d = p.expect("\"describe\"");
    assert!(d.contains("\"fibers\":1"), "{d}");
    p.send(r#"{"t":"shutdown"}"#);
    assert!(p.wait_exit());
}

#[test]
fn nothing_but_protocol_goes_to_standard_output() {
    let mut p = Proc::start();
    p.expect("ready");
    p.send(r#"{"t":"run","source":"trace('a'); trace('b'); mount(() -> <i>x</i>);"}"#);
    p.send(r#"{"t":"shutdown"}"#);
    assert!(p.wait_exit());
    let mut all = Vec::new();
    while let Ok(l) = p.lines.try_recv() {
        all.push(l);
    }
    assert!(!all.is_empty());
    for l in &all {
        assert!(l.starts_with("{\"t\":\"") && l.ends_with('}'), "not a protocol line: {l}");
    }
}
