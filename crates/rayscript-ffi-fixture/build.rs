use std::{env, path::PathBuf, process::Command};

fn main() {
    let target = env::var("TARGET").unwrap();
    let name = if target.contains("windows") {
        "rayscript_ffi_fixture.dll"
    } else if target.contains("apple") {
        "librayscript_ffi_fixture.dylib"
    } else {
        "librayscript_ffi_fixture.so"
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join(name);
    let status = Command::new(env::var_os("RUSTC").unwrap())
        .args([
            "--crate-name",
            "rayscript_ffi_fixture",
            "--crate-type",
            "cdylib",
            "--edition=2024",
            "--target",
            &target,
            "-C",
        ])
        .arg(format!("opt-level={}", env::var("OPT_LEVEL").unwrap()))
        .args(["src/native.rs", "-o"])
        .arg(&output)
        .status()
        .expect("build native-call fixture");
    assert!(status.success(), "native-call fixture did not build");
    println!("cargo:rerun-if-changed=src/native.rs");
    println!("cargo:rustc-env=RAYSCRIPT_FFI_FIXTURE={}", output.display());
}
