//! `rayscript`: run a script, check it, or look at what the parser and compiler made of it.

use rayscript_syntax::dump::dump_module;
use rayscript_vm::bytecode::disassemble_module;
use rayscript_vm::{ModuleResolver, Resolved, Vm};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: rayscript <command> <file.rays>

  run     run the script; its top-level `return` is printed
  check   report what is wrong with it, running nothing
  parse   print the syntax tree
  dump    print the bytecode
";

/// `import a.b.C` finds `a/b/C.rays` beside the script that is running.
struct Files {
    root: PathBuf,
}

impl ModuleResolver for Files {
    fn resolve(&mut self, _vm: &mut Vm, path: &str) -> Option<Resolved> {
        let file = self.root.join(format!("{}.rays", path.replace('.', "/")));
        let source = std::fs::read_to_string(&file).ok()?;
        Some(Resolved::Script { name: file.display().to_string(), source })
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(command), Some(file)) = (args.first(), args.get(1)) else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rayscript: cannot read {file}: {e}");
            return ExitCode::from(2);
        }
    };
    let parsed = rayscript_syntax::parse(&source);
    for d in &parsed.diagnostics {
        eprint!("{}", d.render(file, &source));
    }
    if parsed.has_errors() {
        return ExitCode::FAILURE;
    }
    match command.as_str() {
        "parse" => {
            println!("{}", dump_module(&parsed.module));
            ExitCode::SUCCESS
        }
        "check" | "dump" | "run" => {
            let module = match rayscript_vm::compile(file, &source, &parsed.module) {
                Ok(m) => m,
                Err(diags) => {
                    for d in &diags {
                        eprint!("{}", d.render(file, &source));
                    }
                    return ExitCode::FAILURE;
                }
            };
            match command.as_str() {
                "dump" => {
                    println!("{}", disassemble_module(&module));
                    ExitCode::SUCCESS
                }
                "check" => ExitCode::SUCCESS,
                _ => {
                    let mut vm = Vm::new();
                    let root = Path::new(file).parent().map(Path::to_path_buf).unwrap_or_default();
                    vm.set_resolver(Box::new(Files { root }));
                    match vm.run_source(file, &source) {
                        Ok(v) => {
                            if !v.is_nil() {
                                println!("{}", vm.display(v));
                            }
                            ExitCode::SUCCESS
                        }
                        Err(e) => {
                            eprint!("{e}");
                            ExitCode::FAILURE
                        }
                    }
                }
            }
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
