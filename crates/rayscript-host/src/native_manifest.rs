//! Native grants arrive only from the editor, before any script runs.

use rayscript_vm::{Value, Vm, object::Kind};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub(crate) struct Grant {
    pub name: String,
    pub path: PathBuf,
}

fn text(vm: &Vm, value: Value, field: &str) -> Result<String, String> {
    vm.get_member(value, field)
        .and_then(|v| vm.string_of(v))
        .map(str::to_owned)
        .ok_or_else(|| format!("native configuration requires string `{field}`"))
}

fn array(vm: &Vm, value: Value, field: &str) -> Result<Vec<Value>, String> {
    match vm.get_member(value, field) {
        Some(Value::Obj(o)) if o.kind() == Kind::Array => Ok(o.array().items.clone()),
        _ => Err(format!("native configuration requires array `{field}`")),
    }
}

pub(crate) fn prepare(vm: &Vm, msg: Value) -> Result<(tempfile::TempDir, Vec<Grant>), String> {
    let root = fs::canonicalize(text(vm, msg, "root")?).map_err(|e| format!("native root: {e}"))?;
    if !root.is_dir() {
        return Err("native root must be a directory".into());
    }
    let capabilities: HashSet<String> = array(vm, msg, "capabilities")?
        .into_iter()
        .map(|v| {
            vm.string_of(v)
                .map(str::to_owned)
                .ok_or_else(|| "native capabilities must be strings".to_owned())
        })
        .collect::<Result<_, _>>()?;
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    let mut paths = HashSet::new();
    for value in array(vm, msg, "libraries")? {
        let name = text(vm, value, "name")?;
        if name.is_empty()
            || !name.bytes().enumerate().all(|(i, c)| {
                c.is_ascii_alphabetic() || c == b'_' || i > 0 && (c.is_ascii_digit() || c == b'-')
            })
        {
            return Err(format!("invalid native library name `{name}`"));
        }
        if !names.insert(name.clone()) {
            return Err(format!("duplicate native library `{name}`"));
        }
        if !capabilities.contains(&format!("native:{name}")) {
            return Err(format!(
                "native library `{name}` requires capability native:{name}"
            ));
        }
        let path = text(vm, value, "path")?;
        let relative = Path::new(&path);
        if path.is_empty()
            || path.contains(['\\', ':'])
            || path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(format!(
                "native library `{name}` requires a relative path without traversal"
            ));
        }
        if !paths.insert(path.clone()) {
            return Err(format!("duplicate native library path `{path}`"));
        }
        let hash = text(vm, value, "sha256")?;
        if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("native library `{name}` requires a SHA-256 hash"));
        }
        let source = fs::canonicalize(root.join(relative))
            .map_err(|e| format!("native library `{name}`: {e}"))?;
        if !source.starts_with(&root) || !source.is_file() {
            return Err(format!(
                "native library `{name}` escapes its extension root or is not a file"
            ));
        }
        entries.push((name, path, source, hash));
    }
    for capability in &capabilities {
        if let Some(name) = capability.strip_prefix("native:") {
            if !names.contains(name) {
                return Err(format!(
                    "capability `{capability}` has no native declaration"
                ));
            }
        }
    }
    let directory = tempfile::Builder::new()
        .prefix("rayscript-natives-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let mut grants = Vec::new();
    for (name, relative, source, expected) in entries {
        let path = directory.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut input = File::open(&source).map_err(|e| format!("native library `{name}`: {e}"))?;
        if !input.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err(format!("native library `{name}` is not a regular file"));
        }
        // Hash the private copy: later changes to the installed file cannot change what dlopen reads.
        let mut output = File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            output
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
        let actual = format!("{:x}", hash.finalize());
        if actual != expected.to_ascii_lowercase() {
            return Err(format!(
                "native library `{name}` SHA-256 mismatch: expected {expected}, got {actual}"
            ));
        }
        drop(output);
        let mut permissions = fs::metadata(&path)
            .map_err(|e| e.to_string())?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).map_err(|e| e.to_string())?;
        grants.push(Grant { name, path });
    }
    Ok((directory, grants))
}
