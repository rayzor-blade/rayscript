use rayscript_syntax::dump::dump_module;
use rayscript_syntax::parse;

/// The tree of a script that parses without a report.
fn tree(src: &str) -> String {
    let p = parse(src);
    let reports: Vec<String> = p.diagnostics.iter().map(|d| d.render("t", src)).collect();
    assert!(reports.is_empty(), "unexpected diagnostics for {src:?}:\n{}", reports.join("\n"));
    dump_module(&p.module)
}

fn errors(src: &str) -> Vec<String> {
    parse(src).diagnostics.iter().map(|d| d.message.clone()).collect()
}

fn has_error(src: &str, needle: &str) {
    let e = errors(src);
    assert!(e.iter().any(|m| m.contains(needle)), "expected an error containing {needle:?} for {src:?}, got {e:?}");
}

#[test]
fn arithmetic_follows_haxe_precedence() {
    assert_eq!(tree("1 + 2 * 3;"), "(+ 1 (* 2 3))");
    assert_eq!(tree("a - b - c;"), "(- (- a b) c)");
    assert_eq!(tree("a * b + c;"), "(+ (* a b) c)");
    assert_eq!(tree("a << 1 + 2;"), "(<< a (+ 1 2))");
    // Bitwise operators bind tighter than comparison, as in Haxe.
    assert_eq!(tree("a | b == c;"), "(== (| a b) c)");
    assert_eq!(tree("a && b || c && d;"), "(|| (&& a b) (&& c d))");
    assert_eq!(tree("a == b && c < d;"), "(&& (== a b) (< c d))");
}

#[test]
fn assignment_ternary_and_coalesce() {
    assert_eq!(tree("a = b = c;"), "(= a (= b c))");
    assert_eq!(tree("a += 1;"), "(+= a 1)");
    assert_eq!(tree("a ??= 2;"), "(??= a 2)");
    assert_eq!(tree("x ? y : z ? 1 : 2;"), "(?: x y (?: z 1 2))");
    assert_eq!(tree("a ?? b ?? c;"), "(?? a (?? b c))");
    assert_eq!(tree("a = b ? 1 : 2;"), "(= a (?: b 1 2))");
    assert_eq!(tree("a...b;"), "(... a b)");
    assert_eq!(tree("0...n + 1;"), "(... 0 (+ n 1))");
}

#[test]
fn unary_and_postfix() {
    assert_eq!(tree("-1;"), "-1");
    assert_eq!(tree("-x;"), "(neg x)");
    assert_eq!(tree("-a * b;"), "(* (neg a) b)");
    assert_eq!(tree("!a == b;"), "(== (not a) b)");
    assert_eq!(tree("~a;"), "(bitnot a)");
    assert_eq!(tree("a++ + ++b;"), "(+ (post++ a) (pre++ b))");
    assert_eq!(tree("a.b.c(1, 2);"), "(call (. (. a b) c) 1 2)");
    assert_eq!(tree("a[1][2];"), "([] ([] a 1) 2)");
    assert_eq!(tree("a?.b?.c;"), "(?. (?. a b) c)");
    assert_eq!(tree("f(1)(2);"), "(call (call f 1) 2)");
}

#[test]
fn literals() {
    assert_eq!(tree("0xFF;"), "255");
    assert_eq!(tree("1_000;"), "1000");
    assert_eq!(tree("1.5e2;"), "150.0");
    assert_eq!(tree("true && !false;"), "(&& true (not false))");
    assert_eq!(tree("null;"), "null");
    assert_eq!(tree("\"a\\nb\\t\\u0041\\x41\";"), "\"a\\nb\\tAA\"");
    // The interval's dots are not a number's.
    assert_eq!(tree("1...3;"), "(... 1 3)");
}

#[test]
fn strings_interpolate_in_single_quotes_only() {
    assert_eq!(tree("'a $x b ${y + 1}';"), "(interp \"a \" x \" b \" (+ y 1))");
    assert_eq!(tree("'no interpolation';"), "\"no interpolation\"");
    assert_eq!(tree("\"$x\";"), "\"$x\"");
    assert_eq!(tree("'$$ and $';"), "\"$ and $\"");
    assert_eq!(tree("'${f({a: 1})}';"), "(interp (call f (object (a 1))))");
    assert_eq!(tree("'${'inner $z'}';"), "(interp (interp \"inner \" z))");
}

#[test]
fn declarations() {
    assert_eq!(tree("var a = 1, b:Int = 2;"), "(var (a 1) (b:Int 2))");
    assert_eq!(tree("final c = 3;"), "(final (c 3))");
    assert_eq!(tree("var d;"), "(var (d))");
    assert_eq!(tree("function add(a:Int, b = 2):Int { return a + b; }"), "(fn add (a:Int b=2):Int (block (return (+ a b))))");
    assert_eq!(tree("function f(?a, ...rest) {}"), "(fn f (?a ...rest) (block))");
    assert_eq!(tree("@:blocking function f() {}"), "(@:blocking fn f () (block))");
    assert_eq!(tree("public static inline function f() {}"), "(public static inline fn f () (block))");
    assert_eq!(tree("var f = function(x) { return x; };"), "(var (f (fn _ (x) (block (return x)))))");
}

#[test]
fn types_are_parsed_for_tools() {
    assert_eq!(tree("var a:Array<Array<Int>>;"), "(var (a:Array<Array<Int>>))");
    assert_eq!(tree("var m:Map<String, Int>;"), "(var (m:Map<String,Int>))");
    assert_eq!(tree("var f:Int -> Int;"), "(var (f:(Int)->Int))");
    assert_eq!(tree("var g:(Int, String) -> Bool;"), "(var (g:(Int,String)->Bool))");
    assert_eq!(tree("var o:{a:Int, ?b:String};"), "(var (o:{a:Int,?b:String}))");
    assert_eq!(tree("var p:?Int;"), "(var (p:?Int))");
    assert_eq!(tree("var q:ui.Button;"), "(var (q:ui.Button))");
}

#[test]
fn lambdas() {
    assert_eq!(tree("x -> x * 2;"), "(lambda (x) (* x 2))");
    assert_eq!(tree("(a, b) -> a + b;"), "(lambda (a b) (+ a b))");
    assert_eq!(tree("() -> { f(); };"), "(lambda () (block (call f)))");
    assert_eq!(tree("(a:Int) -> a;"), "(lambda (a:Int) a)");
    assert_eq!(tree("xs.map(x -> x + 1);"), "(call (. xs map) (lambda (x) (+ x 1)))");
    assert_eq!(tree("f(() -> 1, 2);"), "(call f (lambda () 1) 2)");
    // A parenthesis that is not a lambda's parameters.
    assert_eq!(tree("(a + b) * c;"), "(* (paren (+ a b)) c)");
    assert_eq!(tree("(a);"), "(paren a)");
    assert_eq!(tree("() -> {a: 1};"), "(lambda () (object (a 1)))");
}

#[test]
fn control_flow() {
    assert_eq!(tree("if (a) b; else c;"), "(if a b c)");
    assert_eq!(tree("if (a) { b(); } else if (c) { d(); } else { e(); }"), "(if a (block (call b)) (if c (block (call d)) (block (call e))))");
    assert_eq!(tree("while (a) b();"), "(while a (call b))");
    assert_eq!(tree("do { a(); } while (b);"), "(do (block (call a)) b)");
    assert_eq!(tree("for (i in 0...10) f(i);"), "(for i (... 0 10) (call f i))");
    assert_eq!(tree("for (k => v in m) {}"), "(for k=>v m (block))");
    assert_eq!(tree("try { a(); } catch (e:String) { b(); } catch (e) { c(); }"), "(try (block (call a)) (catch e:String (block (call b))) (catch e (block (call c))))");
    assert_eq!(tree("return;"), "(return)");
    assert_eq!(tree("return 1 + 2;"), "(return (+ 1 2))");
    assert_eq!(tree("throw 'bad';"), "(throw \"bad\")");
    assert_eq!(tree("while (true) { if (x) break; else continue; }"), "(while true (block (if x (break) (continue))))");
}

#[test]
fn switch_with_patterns() {
    let src = "switch (x) { case 1, 2: a; case [p, q, ...r] if (p > 0): b; case {x: 1}: c; case Some(v): d; case _: e; default: f; }";
    assert_eq!(
        tree(src),
        "(switch x (case (1 | 2) a) (case ([p q ...r]) if (paren (> p 0)) b) (case ({x:1}) c) (case (Some(v)) d) (case (_) e) (default f))"
    );
    assert_eq!(tree("switch (n) { case -1: a; case 'x' | 'y': b; }"), "(switch n (case (-1) a) (case (\"x\" | \"y\") b))");
    assert_eq!(tree("switch (k) { case ui.Kind.Button: 1; case Red: 2; case other: 3; }"), "(switch k (case (ui.Kind.Button) 1) (case (Red) 2) (case (other) 3))");
    // A case with several statements is a block.
    assert_eq!(tree("switch (x) { case 1: a(); b(); default: c(); }"), "(switch x (case (1) (block (call a) (call b))) (default (call c)))");
}

#[test]
fn collections() {
    assert_eq!(tree("[1, 2, 3];"), "(array 1 2 3)");
    assert_eq!(tree("[];"), "(array)");
    assert_eq!(tree("[\"a\" => 1, \"b\" => 2];"), "(map (\"a\" 1) (\"b\" 2))");
    assert_eq!(tree("[for (x in xs) x * 2];"), "(comp array (for x xs (* x 2)))");
    assert_eq!(tree("[for (x in xs) if (x > 1) x];"), "(comp array (for x xs (if (> x 1) x)))");
    assert_eq!(tree("[for (x in xs) x => x * 2];"), "(comp map (for x xs (map (x (* x 2)))))");
    assert_eq!(tree("var o = {a: 1, \"b\": 2, default: 3};"), "(var (o (object (a 1) (b 2) (default 3))))");
    assert_eq!(tree("var e = {};"), "(var (e (object)))");
    assert_eq!(tree("{ a(); b(); }"), "(block (call a) (call b))");
    assert_eq!(tree("new Foo.Bar(1, 2);"), "(new Foo.Bar 1 2)");
    assert_eq!(tree("new Foo();"), "(new Foo)");
}

#[test]
fn modules() {
    assert_eq!(tree("import a.b.C;"), "(import a.b.C)");
    assert_eq!(tree("import a.b.C as D;"), "(import a.b.C as D)");
    assert_eq!(tree("import a.b.*;"), "(import a.b.*)");
    assert_eq!(tree("native \"libm\" { function cos(x:Float):Float; @:blocking function sleep(ms:Int); }"), "(native \"libm\" (fn cos (x:Float) Float) (fn sleep (ms:Int) @:blocking))");
    assert_eq!(tree("var x = await fetch(u);"), "(var (x (await (call fetch u))))");
}

#[test]
fn markup_elements() {
    assert_eq!(tree("<div class=\"a\" id={x}>hi {name}!</div>;"), "(markup <div @class=\"a\" @id=x> (text \"hi \") (hole name) (text \"!\") </div>)");
    assert_eq!(tree("<input disabled value=\"x\" />;"), "(markup <input @disabled @value=\"x\" />)");
    assert_eq!(tree("<><a/><b/></>;"), "(markup <> <a /> <b /> </>)");
    assert_eq!(tree("return <div/>;"), "(return (markup <div />))");
    assert_eq!(tree("<tab-list><ui.my-card x={1} /></tab-list>;"), "(markup <tab-list> <ui.my-card @x=1 /> </tab-list>)");
    assert_eq!(tree("<Counter start={0} {...props} />;"), "(markup <Counter @start=0 ...props />)");
    assert_eq!(tree("<button onClick={() -> count++}>+</button>;"), "(markup <button @onClick=(lambda () (post++ count))> (text \"+\") </button>)");
    assert_eq!(tree("<a title='t $x'/>;"), "(markup <a @title=(interp \"t \" x) />)");
    assert_eq!(tree("<p>1 &lt; 2 &amp; &lbrace;x&rbrace;</p>;"), "(markup <p> (text \"1 < 2 & {x}\") </p>)");
    assert_eq!(tree("<p>$5 is {price}</p>;"), "(markup <p> (text \"$5 is \") (hole price) </p>)");
}

#[test]
fn markup_whitespace_follows_jsx() {
    assert_eq!(tree("<div>\n    <a/>\n    text here\n</div>;"), "(markup <div> <a /> (text \"text here\") </div>)");
    assert_eq!(tree("<p><b>a</b> <i>b</i></p>;"), "(markup <p> <b> (text \"a\") </b> (text \" \") <i> (text \"b\") </i> </p>)");
    assert_eq!(tree("<p>\n  one\n  two\n</p>;"), "(markup <p> (text \"one two\") </p>)");
}

#[test]
fn markup_control_tags() {
    assert_eq!(tree("<if {a}>x<else>y</if>;"), "(markup (if a (then (text \"x\")) (else (text \"y\"))))");
    assert_eq!(tree("<if {a > 1}><b/></if>;"), "(markup (if (> a 1) (then <b />)))");
    assert_eq!(tree("<for {x in xs}><li>{x}</li></for>;"), "(markup (for x xs (body <li> (hole x) </li>)))");
    assert_eq!(tree("<for {k => v in m}>{k}</for>;"), "(markup (for k=>v m (body (hole k))))");
    assert_eq!(tree("<ul><for {i in 0...3}><li>{i}</li></for></ul>;"), "(markup <ul> (for i (... 0 3) (body <li> (hole i) </li>)) </ul>)");
}

#[test]
fn a_less_than_is_not_markup_after_a_value() {
    assert_eq!(tree("a < b;"), "(< a b)");
    assert_eq!(tree("x = a < b ? 1 : 2;"), "(= x (?: (< a b) 1 2))");
    assert_eq!(tree("f(a < b, c > d);"), "(call f (< a b) (> c d))");
}

#[test]
fn markup_holes_take_any_expression() {
    assert_eq!(tree("<p>{items.map(i -> <li>{i}</li>)}</p>;"), "(markup <p> (hole (call (. items map) (lambda (i) (markup <li> (hole i) </li>)))) </p>)");
    assert_eq!(tree("<p>{ {a: 1} }</p>;"), "(markup <p> (hole (object (a 1))) </p>)");
    assert_eq!(tree("<p>{x}{y}</p>;"), "(markup <p> (hole x) (hole y) </p>)");
}

#[test]
fn a_whole_component() {
    let src = "
import ashui.ui.Button;

function Counter(props) {
    var count = signal(props.start ?? 0);
    return <div class=\"flex gap-2\">
        <text>{count.get()} items</text>
        <button onClick={() -> count.set(count.get() + 1)}>Add</button>
        <if {count.get() > 3}><text>That is plenty</text></if>
    </div>;
}
";
    let t = tree(src);
    assert!(t.starts_with("(import ashui.ui.Button)\n(fn Counter (props) (block (var (count (call signal (?? (. props start) 0))))"), "{t}");
    assert!(t.contains("(markup <div @class=\"flex gap-2\">"), "{t}");
    assert!(t.contains("(if (> (call (. count get)) 3) (then <text> (text \"That is plenty\") </text>))"), "{t}");
}

#[test]
fn comments_and_newlines() {
    assert_eq!(tree("// one\n1 + /* two */ 2; /* three */"), "(+ 1 2)");
    assert_eq!(tree("a\n.b\n.c();"), "(call (. (. a b) c))");
}

#[test]
fn semicolons_may_be_left_out_where_nothing_is_ambiguous() {
    assert_eq!(tree("if (a) { b(); } c();"), "(if a (block (call b)))\n(call c)");
    assert_eq!(tree("function f() {} g();"), "(fn f () (block))\n(call g)");
    assert_eq!(tree("{ a() }"), "(block (call a))");
    assert_eq!(tree("a()"), "(call a)");
}

#[test]
fn errors_say_what_was_expected() {
    has_error("1 +;", "expected an expression, found `;`");
    has_error("a b;", "expected `;` before `b`");
    has_error("{ a();", "never closed");
    has_error("f(1, 2;", "expected `)`");
    has_error("var = 1;", "expected a variable name");
    has_error("<a></b>;", "this closes `</b>`, but the open tag is `<a>`");
    has_error("<div>", "`<div>` is never closed");
    has_error("<>", "`<>` is never closed");
    has_error("<a></>;", "closed with `</>`");
    has_error("<else>x</else>;", "only for inside `<if>`");
    has_error("<input value= />;", "expected a value after `=`");
    has_error("<p>{x</p>;", "expected `}`");
    has_error("try { a(); }", "has no `catch`");
    has_error("'abc", "never closed");
    has_error("\"abc", "never closed");
    has_error("/* abc", "never closed");
    has_error("1 = 2;", "cannot assign to this");
    has_error("a # b;", "is not part of the language");
    has_error("switch (x) { 1: a; }", "expected `case` or `default`");
    has_error("native lib { }", "name of a library in quotes");
    has_error("'${a +}';", "expected an expression");
    has_error("\"\\q\";", "is not an escape");
}

#[test]
fn errors_point_at_the_place() {
    let src = "var a = 1;\nvar b = ;\n";
    let p = parse(src);
    assert_eq!(p.diagnostics.len(), 1);
    let rendered = p.diagnostics[0].render("t.rays", src);
    assert!(rendered.starts_with("Error: expected an expression, found `;`"), "{rendered}");
    assert!(rendered.contains("t.rays:2:9"), "{rendered}");
    assert!(rendered.contains("var b = ;"), "{rendered}");
}

#[test]
fn parsing_goes_on_after_an_error() {
    let p = parse("var a = ; var b = 2; f(");
    let module = dump_module(&p.module);
    assert!(module.contains("(var (b 2))"), "{module}");
    assert!(p.has_errors());
    let p = parse("function f() { var x = ; return 1; }\nvar y = 3;");
    let module = dump_module(&p.module);
    assert!(module.contains("(return 1)") && module.contains("(var (y 3))"), "{module}");
}

#[test]
fn deep_nesting_is_reported_not_overflowed() {
    let src = format!("{}1{}", "(".repeat(5000), ")".repeat(5000));
    let e = errors(&src);
    assert!(e.iter().any(|m| m.contains("nested too deeply")), "{e:?}");
    let src = "[".repeat(5000);
    assert!(parse(&src).has_errors());
    let src = "<a>".repeat(3000);
    assert!(parse(&src).has_errors());
}

/// Whatever a half-typed script looks like, the parser answers, and quickly.
#[test]
fn every_prefix_of_a_script_parses_without_panicking() {
    let sample = "import a.b.C as D;\nnative \"m\" { function f(a:Int):Int; }\n@:meta function g(a:Int = 1, ...r):Void { var x = [1, 2 => 3]; return x?.y ?? 'a ${b + 1} c'; }\nfunction C(p) {\n  switch (p) { case [a, ...b] if (a > 1): 1; case {k: v}: 2; default: 3; }\n  for (i in 0...3) try { f(); } catch (e:Dynamic) { throw e; }\n  var m = [for (k => v in p) k => v * 2];\n  return <div class=\"a\" id={p.id} {...p}>\n    <if {p.ok}>yes &lt; <b>{p.n}</b><else>no</if>\n    <for {i in p.items}><li onClick={() -> i++}>{i}</li></for>\n  </div>;\n}\n";
    let mut count = 0;
    for (i, _) in sample.char_indices().chain(std::iter::once((sample.len(), ' '))) {
        let p = parse(&sample[..i]);
        let _ = dump_module(&p.module);
        count += 1;
    }
    assert!(count > 300);
    // And with each character deleted, and each stray piece of syntax inserted.
    for (i, c) in sample.char_indices() {
        let mut s = String::from(&sample[..i]);
        s.push_str(&sample[i + c.len_utf8()..]);
        let _ = parse(&s);
    }
    for piece in ["<", ">", "</", "/>", "{", "}", "(", ")", "[", "]", "'", "\"", "${", "...", "=>", "->", "@", "?"] {
        for (i, _) in sample.char_indices().step_by(7) {
            let mut s = String::from(&sample[..i]);
            s.push_str(piece);
            s.push_str(&sample[i..]);
            let _ = parse(&s);
        }
    }
}

#[test]
fn rendering_copes_with_the_end_of_the_file_and_wide_characters() {
    for src in ["var a =", "var s = \"héllo\" + ;", "var é = ", "f(\u{1F600}"] {
        let p = parse(src);
        assert!(p.has_errors(), "{src:?}");
        for d in &p.diagnostics {
            let plain = d.render("t.rays", src);
            assert!(plain.starts_with("Error: ") && !plain.contains('\x1b'), "{plain}");
            assert!(d.render_with("t.rays", src, true).contains('\x1b'));
        }
    }
}

#[test]
fn unclosed_brackets_and_tags_point_back_at_where_they_opened() {
    let src = "var a = f(1;";
    let d = &parse(src).diagnostics[0];
    assert!(d.render("t.rays", src).contains("the arguments start here"));
    let src = "var a = <div><span>x</b></div>;";
    let d = &parse(src).diagnostics[0];
    assert!(d.render("t.rays", src).contains("`<span>` is opened here"));
    assert_eq!(d.labels.len(), 1);
}

#[test]
fn native_handles_preserve_destructor_and_owner_metadata() {
    let source = r#"native "fixture" {
        @:free(close_session) handle Session;
        function close_session(session:Session):Void;
        @:owner(session) function borrow(session:Session):Ptr;
    }
    var handle = 1;"#;
    let parsed = parse(source);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let rayscript_syntax::ast::Item::Native(block) = &parsed.module.items[0] else { panic!("native block") };
    assert_eq!(block.handles[0].name.name, "Session");
    assert!(matches!(&block.handles[0].meta[0].args[0].kind, rayscript_syntax::ast::ExprKind::Ident(name) if name == "close_session"));
    assert!(matches!(&block.functions[1].meta[0].args[0].kind, rayscript_syntax::ast::ExprKind::Ident(name) if name == "session"));
    for (end, _) in source.char_indices().chain(std::iter::once((source.len(), ' '))) {
        let _ = parse(&source[..end]);
    }
}

#[test]
fn native_structs_parse_nested_fields_and_pointer_links() {
    let source = r#"native "fixture" {
        struct Outer { tag:UInt8; pair:Pair; next:Ptr<Outer>; }
        struct Pair { x:Int32; y:Double; };
        function fill(value:Outer):Void;
    }
    var struct = 1;"#;
    assert_eq!(tree(source), "(native \"fixture\" (struct Outer (tag UInt8) (pair Pair) (next Ptr<Outer>)) (struct Pair (x Int32) (y Double)) (fn fill (value:Outer) Void))\n(var (struct 1))");
    for (end, _) in source.char_indices().chain(std::iter::once((source.len(), ' '))) { let _ = parse(&source[..end]); }
    has_error("native \"fixture\" { @:packed struct Pair { x:Int32; } }", "struct annotations are not supported");
}

#[test]
fn native_callbacks_keep_signatures_and_failure_literals() {
    let source = r#"native "fixture" {
        @:failure(-1) callback Visit(value:Int32, data:Ptr):Int32;
        callback Done():Void;
        struct Options { visit:Visit; }
    }"#;
    let parsed = parse(source);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let rayscript_syntax::ast::Item::Native(block) = &parsed.module.items[0] else { panic!("native block") };
    assert_eq!(block.callbacks.len(), 2);
    assert_eq!(block.callbacks[0].name.name, "Visit");
    assert_eq!(block.callbacks[0].params.len(), 2);
    assert_eq!(block.callbacks[0].meta[0].name, ":failure");
    assert!(tree(source).contains("(callback Visit (value:Int32 data:Ptr) Int32 @:failure)"));
    for (end, _) in source.char_indices().chain(std::iter::once((source.len(), ' '))) { let _ = parse(&source[..end]); }
}
