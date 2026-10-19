use rayscript_host::{Config, Host};
use rayscript_vm::stdlib::json::quote;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn json(value: &str) -> String {
    let mut out = String::new();
    quote(value, &mut out);
    out
}
fn grant(root: &Path, path: &str, hash: &str, capabilities: &str) -> String {
    format!(
        r#"{{"t":"natives","root":{},"capabilities":{capabilities},"libraries":[{{"name":"fixture","path":{},"sha256":{}}}]}}"#,
        json(root.to_str().unwrap()),
        json(path),
        json(hash)
    )
}
fn run(host: &mut Host, source: &str) -> String {
    host.handle_line(&format!(r#"{{"t":"run","source":{}}}"#, json(source)));
    for _ in 0..100 {
        if !host.pump() {
            break;
        }
    }
    host.take_output().join("\n")
}
fn fixture() -> (tempfile::TempDir, String, String) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("natives")).unwrap();
    let name = Path::new(rayscript_ffi_fixture::LIBRARY_PATH)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let path = format!("natives/{name}");
    fs::copy(rayscript_ffi_fixture::LIBRARY_PATH, root.path().join(&path)).unwrap();
    let hash = format!(
        "{:x}",
        Sha256::digest(fs::read(root.path().join(&path)).unwrap())
    );
    (root, path, hash)
}
const ADD: &str = r#"native "fixture" { function add(a:Int64,b:Int64):Int64; } trace(add(20,22));"#;

#[test]
fn verified_grant_runs_copied_bytes_after_install_changes() {
    let (root, path, hash) = fixture();
    let mut host = Host::new(Config::default());
    host.handle_line(&grant(
        root.path(),
        &path,
        &hash.to_uppercase(),
        r#"["native:fixture"]"#,
    ));
    assert!(host.take_output().join("\n").contains("nativesReady"));
    fs::write(root.path().join(path), "replaced after validation").unwrap();
    let output = run(&mut host, ADD);
    assert!(output.contains(r#""message":"42""#), "{output}");
    assert!(!output.contains(r#""t":"error""#), "{output}");
}

#[test]
fn rejected_grants_cannot_run_scripts_or_register_partial_sets() {
    let (root, path, hash) = fixture();
    for (path, hash, caps, expected) in [
        (path.as_str(), hash.as_str(), "[]", "requires capability"),
        (path.as_str(), "00", r#"["native:fixture"]"#, "SHA-256 hash"),
        (
            path.as_str(),
            "0000000000000000000000000000000000000000000000000000000000000000",
            r#"["native:fixture"]"#,
            "SHA-256 mismatch",
        ),
        (
            "../outside",
            hash.as_str(),
            r#"["native:fixture"]"#,
            "relative path",
        ),
        (
            "/tmp/outside",
            hash.as_str(),
            r#"["native:fixture"]"#,
            "relative path",
        ),
        (
            "natives\\outside",
            hash.as_str(),
            r#"["native:fixture"]"#,
            "relative path",
        ),
        (
            "natives/./outside",
            hash.as_str(),
            r#"["native:fixture"]"#,
            "relative path",
        ),
        (
            path.as_str(),
            hash.as_str(),
            r#"["native:fixture","native:undeclared"]"#,
            "no native declaration",
        ),
    ] {
        let mut host = Host::new(Config::default());
        host.handle_line(&grant(root.path(), path, hash, caps));
        let output = run(&mut host, "trace('must not execute');");
        assert!(output.contains(expected), "{output}");
        assert!(output.contains("restart the host"), "{output}");
        assert!(!output.contains("must not execute"), "{output}");
    }
    let mut host = Host::new(Config::default());
    let first = grant(root.path(), &path, &hash, r#"["native:fixture"]"#);
    let invalid = first.replace(
        "]}",
        r#",{"name":"other","path":"missing","sha256":"bad"}]}"#,
    );
    host.handle_line(&invalid);
    assert!(host.vm().run_source("unregistered", ADD).is_err());
}

#[test]
fn grants_are_one_shot_and_precede_any_script_attempt() {
    let (root, path, hash) = fixture();
    let message = grant(root.path(), &path, &hash, r#"["native:fixture"]"#);
    let mut host = Host::new(Config::default());
    assert!(run(&mut host, ADD).contains("not registered"));
    host.handle_line(&message);
    assert!(
        host.take_output()
            .join("\n")
            .contains("must precede scripts")
    );
    let mut host = Host::new(Config::default());
    host.handle_line(&message);
    host.take_output();
    host.handle_line(&message);
    assert!(host.take_output().join("\n").contains("only once"));
    assert!(run(&mut host, ADD).contains(r#""message":"42""#));
}

#[test]
#[cfg(unix)]
fn symlinks_must_resolve_inside_extension() {
    let (root, path, hash) = fixture();
    let outside = tempfile::tempdir().unwrap();
    fs::copy(root.path().join(&path), outside.path().join("library")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("library"), root.path().join("escape")).unwrap();
    let mut host = Host::new(Config::default());
    host.handle_line(&grant(
        root.path(),
        "escape",
        &hash,
        r#"["native:fixture"]"#,
    ));
    assert!(
        host.take_output()
            .join("\n")
            .contains("escapes its extension root")
    );
    std::os::unix::fs::symlink(root.path().join(&path), root.path().join("inside")).unwrap();
    let mut host = Host::new(Config::default());
    host.handle_line(&grant(
        root.path(),
        "inside",
        &hash,
        r#"["native:fixture"]"#,
    ));
    assert!(run(&mut host, ADD).contains(r#""message":"42""#));
}
