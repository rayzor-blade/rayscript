use rayscript_host::wire::decode_ops;
use rayscript_host::{Config, Host};
use rayscript_vm::stdlib::json::{parse, quote};
use rayscript_vm::view::{Dom, PropValue};
use rayscript_vm::{Value, Vm};
use std::time::{Duration, Instant};

/// What a message from the host said, kept with the VM that can read it.
struct Msg {
    t: String,
    value: Value,
}

/// An editor in miniature: it sends lines to a host, reads what comes back, and keeps the elements the
/// host asks for in a model that checks each operation.
struct Editor {
    host: Host,
    reader: Vm,
    dom: Dom,
    got: Vec<Msg>,
    roots: Vec<u32>,
}

fn json(s: &str) -> String {
    let mut out = String::new();
    quote(s, &mut out);
    out
}

impl Editor {
    fn new() -> Editor {
        Editor::with(Config::default())
    }

    fn with(config: Config) -> Editor {
        Editor { host: Host::new(config), reader: Vm::new(), dom: Dom::new(), got: Vec::new(), roots: Vec::new() }
    }

    fn send(&mut self, line: &str) {
        self.host.handle_line(line);
    }

    fn run(&mut self, source: &str) {
        self.send(&format!("{{\"t\":\"run\",\"name\":\"main.rays\",\"source\":{}}}", json(source)));
    }

    /// Lets the host work until it has nothing more to do right now, and reads what it sent.
    fn settle(&mut self) {
        for _ in 0..1000 {
            let more = self.host.pump();
            self.collect();
            if !more {
                break;
            }
        }
    }

    fn collect(&mut self) {
        for line in self.host.take_output() {
            let v = parse(&mut self.reader, &line).unwrap_or_else(|_| panic!("the host sent something that is not JSON: {line}"));
            let t = self.reader.get_member(v, "t").and_then(|t| self.reader.string_of(t)).unwrap_or("").to_string();
            if t == "ops" {
                let ops = self.reader.get_member(v, "ops").unwrap();
                let ops = decode_ops(&mut self.reader, ops).unwrap_or_else(|e| panic!("bad ops: {e}\n{line}"));
                for op in &ops {
                    if let rayscript_vm::view::ViewOp::Create { id, tag } = op {
                        if tag == "#root" {
                            self.roots.push(*id);
                        }
                    }
                }
                if let Err(e) = self.dom.apply(&ops) {
                    panic!("the host asked for something impossible: {e}\n{line}");
                }
            }
            self.got.push(Msg { t, value: v });
        }
    }

    fn html(&self) -> String {
        self.dom.inner(self.roots[0])
    }

    fn of_kind(&self, t: &str) -> Vec<&Msg> {
        self.got.iter().filter(|m| m.t == t).collect()
    }

    fn field(&self, m: &Msg, name: &str) -> Value {
        self.reader.get_member(m.value, name).unwrap_or(Value::Nil)
    }

    fn text(&self, m: &Msg, name: &str) -> String {
        self.reader.string_of(self.field(m, name)).unwrap_or("").to_string()
    }

    fn int(&self, m: &Msg, name: &str) -> i64 {
        match self.field(m, name) {
            Value::Int(n) => n,
            other => panic!("{name} is not a number but {}", self.reader.display(other)),
        }
    }

    fn click(&mut self, tag: &str) {
        let id = self.dom.find(self.roots[0], tag)[0];
        self.send(&format!("{{\"t\":\"event\",\"node\":{id},\"event\":\"click\"}}"));
        self.settle();
    }

    fn no_errors(&self) {
        let errors: Vec<String> = self.of_kind("error").iter().map(|m| self.text(m, "message")).collect();
        assert!(errors.is_empty(), "the host reported errors: {errors:#?}");
    }
}

#[test]
fn a_script_is_run_and_its_view_comes_back_as_operations() {
    let mut e = Editor::new();
    e.run("mount(() -> <p class='x'>hi</p>, 'panel');");
    e.settle();
    e.no_errors();
    assert_eq!(e.html(), "<p class=\"x\">hi</p>");
    assert_eq!(e.dom.prop(e.roots[0], "name"), Some(&PropValue::Str("panel".into())));
}

#[test]
fn an_event_reaches_its_handler_and_the_change_comes_back() {
    let mut e = Editor::new();
    e.run("var n = signal(0); mount(() -> <div><b>{n}</b><button onClick={() -> n.value++}>+</button></div>);");
    e.settle();
    assert_eq!(e.html(), "<div><b>0</b><button>+</button></div>");
    let before = e.got.len();
    e.click("button");
    e.click("button");
    assert_eq!(e.html(), "<div><b>2</b><button>+</button></div>");
    // One operation each click, and nothing else was said.
    assert_eq!(e.got.len() - before, 2);
}

#[test]
fn an_event_carries_its_json_payload() {
    let mut e = Editor::new();
    e.run("var last = signal('-'); mount(() -> <input onInput={(ev) -> last.value = ev.text + '/' + ev.pos}/>); mount(() -> <p>{last}</p>, 'out');");
    e.settle();
    let input = e.dom.find(e.roots[0], "input")[0];
    e.send(&format!("{{\"t\":\"event\",\"node\":{input},\"event\":\"input\",\"payload\":{{\"text\":\"hey\",\"pos\":3}}}}"));
    e.settle();
    assert_eq!(e.dom.inner(e.roots[1]), "<p>hey/3</p>");
    // An element that is not there, or not listening, is not an error.
    e.send("{\"t\":\"event\",\"node\":9999,\"event\":\"click\"}");
    e.send(&format!("{{\"t\":\"event\",\"node\":{input},\"event\":\"nothing\"}}"));
    e.settle();
    e.no_errors();
}

#[test]
fn a_remote_module_is_called_and_answered() {
    let mut e = Editor::new();
    e.send(r#"{"t":"module","path":"rayde.workspace","functions":["say","count"]}"#);
    e.run(
        "import rayde.workspace as ws;
         var out = signal('waiting');
         mount(() -> <p>{out}</p>);
         var n = await ws.count('a', [1, 2], {k: true});
         out.value = 'got ' + n;",
    );
    e.settle();
    let calls = e.of_kind("call");
    assert_eq!(calls.len(), 1);
    let call = calls[0];
    assert_eq!(e.text(call, "fn"), "rayde.workspace.count");
    assert_eq!(e.reader.display(e.field(call, "args")), "[a,[1,2],{k: true}]");
    // What the script had drawn is sent before the call that follows it.
    let ops_at = e.got.iter().position(|m| m.t == "ops").unwrap();
    let call_at = e.got.iter().position(|m| m.t == "call").unwrap();
    assert!(ops_at < call_at);
    assert_eq!(e.html(), "<p>waiting</p>");
    let id = e.int(call, "id");
    e.send(&format!("{{\"t\":\"resolve\",\"id\":{id},\"value\":42}}"));
    e.settle();
    assert_eq!(e.html(), "<p>got 42</p>");
    e.no_errors();
}

#[test]
fn a_call_that_is_rejected_throws_in_the_script() {
    let mut e = Editor::new();
    e.send(r#"{"t":"module","path":"rayde.workspace","functions":["say"]}"#);
    e.run(
        "import rayde.workspace.say;
         var out = signal('waiting');
         mount(() -> <p>{out}</p>);
         try { await say('x'); } catch (err) { out.value = 'failed: ' + err; }",
    );
    e.settle();
    let id = e.int(e.of_kind("call")[0], "id");
    e.send(&format!("{{\"t\":\"reject\",\"id\":{id},\"message\":\"no editor\"}}"));
    e.settle();
    assert_eq!(e.html(), "<p>failed: no editor</p>");
    // An answer to nothing is said so.
    e.send("{\"t\":\"resolve\",\"id\":777}");
    e.settle();
    assert_eq!(e.of_kind("error").len(), 1);
}

#[test]
fn an_unanswered_call_does_not_stop_the_rest() {
    let mut e = Editor::new();
    e.send(r#"{"t":"module","path":"m","functions":["f"]}"#);
    e.run("import m; var n = signal(0); mount(() -> <b onClick={() -> n.value++}>{n}</b>); m.f(1); m.f(2);");
    e.settle();
    assert_eq!(e.of_kind("call").len(), 2);
    e.click("b");
    assert_eq!(e.html(), "<b>1</b>");
    let describe_before = e.got.len();
    e.send("{\"t\":\"describe\"}");
    e.settle();
    let d = &e.got[describe_before];
    assert_eq!(d.t, "describe");
    assert_eq!(e.int(d, "pendingCalls"), 2);
}

#[test]
fn sleep_is_a_timer() {
    let mut e = Editor::new();
    e.run("var out = signal('asleep'); mount(() -> <p>{out}</p>); await sleep(40); out.value = 'awake';");
    e.settle();
    assert_eq!(e.html(), "<p>asleep</p>");
    let due = e.host.next_wake().expect("a timer is waiting");
    assert!(due > Instant::now());
    std::thread::sleep(due.saturating_duration_since(Instant::now()) + Duration::from_millis(2));
    e.settle();
    assert_eq!(e.html(), "<p>awake</p>");
    assert!(e.host.next_wake().is_none());
}

#[test]
fn trace_is_a_log_message_and_not_protocol_noise() {
    let mut e = Editor::new();
    e.run("trace('hello', 1 + 1);");
    e.settle();
    let logs = e.of_kind("log");
    assert_eq!(logs.len(), 1);
    assert_eq!(e.text(logs[0], "message"), "hello 2");
}

#[test]
fn a_script_that_compiles_is_said_to_have_started_and_one_that_does_not_is_not() {
    let mut e = Editor::new();
    e.run("var ok = 1;\nvar = 2;");
    e.settle();
    assert!(e.of_kind("started").is_empty());
    e.run("mount(() -> <i>x</i>);");
    e.settle();
    let started = e.of_kind("started");
    assert_eq!(started.len(), 1);
    assert_eq!(e.text(started[0], "name"), "main.rays");
    // It comes before the operations the script makes.
    let at = e.got.iter().position(|m| m.t == "started").unwrap();
    let ops = e.got.iter().position(|m| m.t == "ops").unwrap();
    assert!(at < ops);
}

#[test]
fn a_syntax_error_comes_with_where() {
    let mut e = Editor::new();
    e.run("var ok = 1;\nvar = 2;");
    e.settle();
    let errors = e.of_kind("error");
    assert_eq!(errors.len(), 1);
    assert_eq!(e.text(errors[0], "kind"), "syntax");
    assert!(e.text(errors[0], "message").contains("expected a variable name"));
    let items = e.field(errors[0], "items");
    let first = items.as_obj().unwrap().array().items[0];
    assert!(matches!(e.reader.get_member(first, "line"), Some(Value::Int(2))));
    assert!(matches!(e.reader.get_member(first, "col"), Some(Value::Int(5))));
    assert_eq!(e.reader.string_of(e.reader.get_member(first, "file").unwrap()), Some("main.rays"));
}

#[test]
fn a_script_that_throws_is_reported_with_its_trace() {
    let mut e = Editor::new();
    e.run("function inner() {\n  throw 'bad thing';\n}\nfunction outer() {\n  inner();\n}\nouter();");
    e.settle();
    let errors = e.of_kind("error");
    assert_eq!(errors.len(), 1, "once");
    assert_eq!(e.text(errors[0], "kind"), "runtime");
    assert!(e.text(errors[0], "message").contains("bad thing"));
    let items = e.field(errors[0], "items").as_obj().unwrap().array().items.clone();
    let functions: Vec<String> = items.iter().map(|i| e.reader.string_of(e.reader.get_member(*i, "function").unwrap()).unwrap().to_string()).collect();
    assert_eq!(functions[..2], ["inner", "outer"]);
    assert!(matches!(e.reader.get_member(items[0], "line"), Some(Value::Int(2))));
    // The host goes on: it still takes another script.
    e.run("mount(() -> <i>fine</i>);");
    e.settle();
    assert_eq!(e.html(), "<i>fine</i>");
}

#[test]
fn a_handler_that_throws_is_reported_as_a_view_error() {
    let mut e = Editor::new();
    e.run("mount(() -> <b onClick={() -> throw 'oops'}>x</b>);");
    e.settle();
    e.click("b");
    let errors = e.of_kind("error");
    assert_eq!(errors.len(), 1);
    assert_eq!(e.text(errors[0], "kind"), "view");
    assert!(e.text(errors[0], "message").contains("oops"));
}

#[test]
fn bad_lines_are_said_so_and_the_host_goes_on() {
    let mut e = Editor::new();
    e.send("this is not json");
    e.send("{\"nothing\":1}");
    e.send("{\"t\":\"warp\"}");
    e.send("{\"t\":\"run\"}");
    e.send("{\"t\":\"module\",\"path\":\"x\"}");
    e.send("{\"t\":\"event\"}");
    e.send("{\"t\":\"resolve\"}");
    e.send("");
    e.settle();
    let errors = e.of_kind("error");
    assert_eq!(errors.len(), 7, "{:?}", errors.iter().map(|m| e.text(m, "message")).collect::<Vec<_>>());
    assert!(errors.iter().all(|m| e.text(m, "kind") == "protocol"));
    e.run("mount(() -> <i>fine</i>);");
    e.settle();
    assert_eq!(e.html(), "<i>fine</i>");
}

#[test]
fn describe_says_what_the_host_is_doing() {
    let mut e = Editor::new();
    e.send(r#"{"t":"module","path":"b.mod","functions":["f"]}"#);
    e.send(r#"{"t":"module","path":"a.mod","functions":["g"]}"#);
    e.run("mount(() -> <ul><li>1</li><li>2</li></ul>, 'list');");
    e.settle();
    e.send("{\"t\":\"describe\"}");
    e.settle();
    let d = e.of_kind("describe")[0];
    assert_eq!(e.int(d, "protocol"), 1);
    assert_eq!(e.int(d, "nodes"), 1 + 1 + 2 + 2);
    assert_eq!(e.reader.display(e.field(d, "remoteModules")), "[a.mod,b.mod]");
    let mounts = e.field(d, "mounts").as_obj().unwrap().array().items.clone();
    assert_eq!(mounts.len(), 1);
    assert_eq!(e.reader.string_of(e.reader.get_member(mounts[0], "name").unwrap()), Some("list"));
    assert!(e.int(d, "opsSent") > 0);
}

#[test]
fn script_modules_are_imported_from_the_root_and_nowhere_else() {
    let dir = std::env::temp_dir().join(format!("rayscript-host-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("ui")).unwrap();
    std::fs::write(dir.join("ui/Badge.rays"), "function Badge(props) { return <span class='badge'>{props.text}</span>; }").unwrap();
    let mut e = Editor::with(Config { root: Some(dir.clone()), ..Config::default() });
    e.run("import ui.Badge.Badge; mount(() -> <div><Badge text='new'/></div>);");
    e.settle();
    e.no_errors();
    assert_eq!(e.html(), "<div><span class=\"badge\">new</span></div>");
    e.run("import ui.Missing; mount(() -> <b>never</b>, 'x');");
    e.settle();
    assert_eq!(e.of_kind("error").len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn shutdown_ends_the_host() {
    let mut e = Editor::new();
    assert!(!e.host.is_shutdown());
    e.send("{\"t\":\"shutdown\"}");
    assert!(e.host.is_shutdown());
}

#[test]
fn an_effect_that_never_ends_is_stopped_and_the_host_goes_on() {
    let mut e = Editor::with(Config { call_fuel: 50_000, ..Config::default() });
    e.run("var s = signal(0); var n = signal(0); effect(() -> { if (s.value == 1) { while (true) { } } }); mount(() -> <div><b onClick={() -> s.value = 1}>go</b><i onClick={() -> n.value++}>{n}</i></div>);");
    e.settle();
    e.click("b");
    let errors = e.of_kind("error");
    assert_eq!(errors.len(), 1);
    assert_eq!(e.text(errors[0], "kind"), "view");
    assert!(e.text(errors[0], "message").contains("ran for more than 50000 steps"));
    // The host still takes events.
    e.click("i");
    assert_eq!(e.html(), "<div><b>go</b><i>1</i></div>");
}

#[test]
fn a_spawned_fiber_that_fails_is_reported() {
    let mut e = Editor::new();
    e.run("spawn(() -> { throw 'lost in the background'; });");
    e.settle();
    let errors = e.of_kind("error");
    assert_eq!(errors.len(), 1);
    assert_eq!(e.text(errors[0], "kind"), "runtime");
    assert!(e.text(errors[0], "message").contains("lost in the background"));
}

#[test]
fn a_tool_of_the_header_runs_the_handler_the_script_gave() {
    let mut e = Editor::new();
    e.run("var n = signal(0); onTool('add', () -> n.value++); mount(() -> <b>{n}</b>);");
    e.settle();
    assert_eq!(e.html(), "<b>0</b>");
    e.send("{\"t\":\"tool\",\"id\":\"add\"}");
    e.send("{\"t\":\"tool\",\"id\":\"add\"}");
    e.settle();
    assert_eq!(e.html(), "<b>2</b>");
    e.no_errors();
    // A tool nothing answers is said so, with how to answer it.
    e.send("{\"t\":\"tool\",\"id\":\"nope\"}");
    e.send("{\"t\":\"tool\"}");
    e.settle();
    let errors = e.of_kind("error");
    assert_eq!(errors.len(), 2);
    assert!(e.text(errors[0], "message").contains("onTool('nope'"), "{}", e.text(errors[0], "message"));
}

#[test]
fn a_script_can_title_its_view() {
    let mut e = Editor::new();
    e.run("setTitle('Notes (3)'); await sleep(1); setTitle('He said \"hi\"');");
    e.settle();
    std::thread::sleep(std::time::Duration::from_millis(5));
    e.settle();
    let titles: Vec<String> = e.of_kind("view").iter().map(|m| e.text(m, "title")).collect();
    assert_eq!(titles, ["Notes (3)", "He said \"hi\""]);
}
