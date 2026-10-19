#![cfg(unix)]

use rayscript_host::{Config, Host, serve_host};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

#[test]
fn native_callbacks_and_completions_wake_an_idle_protocol_loop() {
    let (mut editor, server) = UnixStream::pair().unwrap();
    editor
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let run = std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(move || {
            let mut host = Host::new(Config::default());
            unsafe {
                host.vm()
                    .register_native_library("fixture", rayscript_ffi_fixture::LIBRARY_PATH);
            }
            host.vm()
                .start_source(
                    "wake.rays",
                    r#"
            native "fixture" {
                @:failure(-7) callback Visit(value:Int32,data:Ptr):Int32;
                @:blocking function visit(cb:Visit,data:Ptr,count:Int32):Int32;
            }
            visit(Visit((v,d) -> { setTitle('native callback'); return 0; }), null, 1);
            setTitle('native done');
        "#,
                )
                .unwrap();
            serve_host(BufReader::new(server.try_clone().unwrap()), server, host).unwrap();
        })
        .unwrap();
    let mut output = BufReader::new(editor.try_clone().unwrap());
    let mut callback = false;
    loop {
        let mut line = String::new();
        assert!(output.read_line(&mut line).unwrap() > 0);
        if line.contains("native callback") {
            callback = true;
        }
        if line.contains("native done") {
            break;
        }
    }
    assert!(callback);
    writeln!(editor, "{{\"t\":\"shutdown\"}}").unwrap();
    editor.shutdown(std::net::Shutdown::Write).unwrap();
    run.join().unwrap();
}
