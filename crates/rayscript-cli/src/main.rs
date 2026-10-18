//! `rayscript`: run a script, check it, or look at what the parser and compiler made of it.

use rayscript_syntax::dump::dump_module;
use rayscript_syntax::render_all;
use rayscript_vm::bytecode::disassemble_module;
use rayscript_vm::view::{Dom, ViewOp};
use rayscript_vm::{ModuleResolver, Resolved, Vm};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: rayscript <command> <file.rays>

  run     run the script; its top-level `return` is printed
  check   report what is wrong with it, running nothing
  parse   print the syntax tree
  dump    print the bytecode
  view    run the script, then print the elements its views made
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
    // Colour and line drawing for a person at a terminal; plain text for a pipe or a log.
    let fancy = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let parsed = rayscript_syntax::parse(&source);
    eprint!("{}", render_all(&parsed.diagnostics, file, &source, fancy));
    if parsed.has_errors() {
        return ExitCode::FAILURE;
    }
    match command.as_str() {
        "parse" => {
            println!("{}", dump_module(&parsed.module));
            ExitCode::SUCCESS
        }
        "check" | "dump" | "run" | "view" => {
            let module = match rayscript_vm::compile(file, &source, &parsed.module) {
                Ok(m) => m,
                Err(diags) => {
                    eprint!("{}", render_all(&diags, file, &source, fancy));
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
                    vm.set_fancy_errors(fancy);
                    let root = Path::new(file).parent().map(Path::to_path_buf).unwrap_or_default();
                    vm.set_resolver(Box::new(Files { root }));
                    match vm.run_source(file, &source) {
                        Ok(v) => {
                            if !v.is_nil() {
                                println!("{}", vm.display(v));
                            }
                            vm.view_pump(1_000_000);
                            let ops = vm.view_take_ops();
                            let errors = vm.view_take_errors();
                            for e in &errors {
                                eprint!("{e}");
                            }
                            if command == "view" {
                                let mut dom = Dom::new();
                                if let Err(e) = dom.apply(&ops) {
                                    eprintln!("rayscript: {e}");
                                    return ExitCode::FAILURE;
                                }
                                for op in &ops {
                                    if let ViewOp::Create { id, tag } = op {
                                        if tag == "#root" {
                                            print!("{}", dom.pretty(*id));
                                        }
                                    }
                                }
                            }
                            if errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
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
