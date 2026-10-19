use rayscript_vm::heap::HeapConfig;
use rayscript_vm::view::{Dom, ViewOp};
use rayscript_vm::{Value, Vm};

/// A VM and the host model that its operations are applied to. After every settle the runtime's own tree
/// must read the same as the host's.
struct Host {
    vm: Vm,
    dom: Dom,
    roots: Vec<u32>,
    /// The operations of the last settle, as text.
    ops: Vec<String>,
}

impl Host {
    fn new(stress: bool) -> Host {
        let config = HeapConfig { stress, ..HeapConfig::default() };
        Host { vm: Vm::with_config(config), dom: Dom::new(), roots: Vec::new(), ops: Vec::new() }
    }

    fn run(&mut self, src: &str) -> Value {
        let v = match self.vm.run_source("t.rays", src) {
            Ok(v) => v,
            Err(e) => panic!("{e}"),
        };
        self.vm.root(v);
        self.settle();
        v
    }

    fn settle(&mut self) {
        self.vm.view_pump(100_000);
        let errors = self.vm.view_take_errors();
        assert!(errors.is_empty(), "errors in the view: {errors:#?}");
        self.apply();
    }

    fn apply(&mut self) {
        let ops = self.vm.view_take_ops();
        for op in &ops {
            if let ViewOp::Create { id, tag } = op {
                if tag == "#root" {
                    self.roots.push(*id);
                }
            }
        }
        self.ops = ops.iter().map(|o| o.to_string()).collect();
        if let Err(e) = self.dom.apply(&ops) {
            panic!("the host cannot do what the runtime asked: {e}\n{}", self.ops.join("\n"));
        }
        for r in &self.roots {
            assert_eq!(self.vm.view_inner_html(*r), self.dom.inner(*r), "the runtime's tree and the host's differ");
        }
    }

    fn html(&self) -> String {
        self.dom.inner(self.roots[0])
    }

    fn find(&self, tag: &str) -> Vec<u32> {
        self.dom.find(self.roots[0], tag)
    }

    fn click(&mut self, node: u32) {
        assert!(self.vm.view_dispatch(node, "click", Value::Nil), "nothing listens for click on {node}");
        self.settle();
    }

    /// Clicks the nth element with this tag.
    fn click_nth(&mut self, tag: &str, n: usize) {
        let id = self.find(tag)[n];
        self.click(id);
    }
}

/// Runs a test with a normal heap and with a collection at every poll.
fn each(test: impl Fn(&mut Host) + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(32 << 20)
        .spawn(move || {
            for stress in [false, true] {
                let mut host = Host::new(stress);
                test(&mut host);
            }
        })
        .unwrap()
        .join()
        .unwrap_or_else(|e| std::panic::resume_unwind(e));
}

#[test]
fn markup_is_mounted() {
    each(|h| {
        h.run("mount(() -> <div class=\"a\" hidden>hi <b>there</b></div>);");
        assert_eq!(h.html(), "<div class=\"a\" hidden>hi <b>there</b></div>");
    });
}

#[test]
fn a_hole_follows_its_signal_and_nothing_else_is_touched() {
    each(|h| {
        h.run("var n = signal(1); mount(() -> <div><p>count: {n.value}</p><button onClick={() -> n.value++}>+</button></div>);");
        assert_eq!(h.html(), "<div><p>count: 1</p><button>+</button></div>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><p>count: 2</p><button>+</button></div>");
        assert_eq!(h.ops.len(), 1, "{:?}", h.ops);
        assert!(h.ops[0].starts_with("text "), "{:?}", h.ops);
    });
}

#[test]
fn an_attribute_follows_its_signal() {
    each(|h| {
        h.run("var on = signal(false); mount(() -> <button class={on.value ? 'lit' : 'dim'} title='t ${on.value}' onClick={() -> on.value = !on.value}>x</button>);");
        assert_eq!(h.html(), "<button class=\"dim\" title=\"t false\">x</button>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<button class=\"lit\" title=\"t true\">x</button>");
        assert_eq!(h.ops.len(), 2, "{:?}", h.ops);
    });
}

#[test]
fn a_value_that_does_not_change_sends_nothing() {
    each(|h| {
        h.run("var n = signal(1); mount(() -> <div><p>{n.value > 0 ? 'pos' : 'neg'}</p><b onClick={() -> n.value = n.value + 1}>x</b></div>);");
        h.click_nth("b", 0);
        // The hole ran again, found the same text, and said nothing.
        assert!(h.ops.is_empty(), "{:?}", h.ops);
    });
}

#[test]
fn components_take_attributes_and_children() {
    each(|h| {
        h.run(
            "function Card(props) { return <section class='card'><h1>{props.title}</h1>{props.children}</section>; }
             var title = signal('one');
             mount(() -> <div><Card title={title.value}><p>body</p></Card><button onClick={() -> title.value = 'two'}>go</button></div>);",
        );
        assert_eq!(h.html(), "<div><section class=\"card\"><h1>one</h1><p>body</p></section><button>go</button></div>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><section class=\"card\"><h1>two</h1><p>body</p></section><button>go</button></div>");
        assert_eq!(h.ops.len(), 1, "{:?}", h.ops);
    });
}

#[test]
fn a_branch_is_swapped_when_its_condition_flips() {
    each(|h| {
        h.run(
            "var n = signal(0);
             mount(() -> <div><if {n.value > 1}><b>big</b><else><i>small</i></if><button onClick={() -> n.value++}>+</button></div>);",
        );
        assert_eq!(h.html(), "<div><i>small</i><button>+</button></div>");
        h.click_nth("button", 0);
        // 0 to 1: still not above one, so the branch stays.
        assert!(h.ops.is_empty(), "{:?}", h.ops);
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><b>big</b><button>+</button></div>");
        assert!(h.ops.iter().any(|o| o.starts_with("remove ")) && h.ops.iter().any(|o| o.starts_with("insert ")), "{:?}", h.ops);
    });
}

#[test]
fn a_list_keeps_the_elements_of_items_that_stay() {
    each(|h| {
        h.run(
            "var items = signal(['a', 'b', 'c']);
             mount(() -> <div><ul><for {x in items.value}><li>{x}</li></for></ul>
               <button onClick={() -> items.value = ['a', 'c', 'd']}>go</button></div>);",
        );
        assert_eq!(h.html(), "<div><ul><li>a</li><li>b</li><li>c</li></ul><button>go</button></div>");
        let before = h.find("li");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><ul><li>a</li><li>c</li><li>d</li></ul><button>go</button></div>");
        let after = h.find("li");
        // a and c are the same elements; b went and d came.
        assert_eq!((after[0], after[1]), (before[0], before[2]));
        assert_ne!(after[2], before[1]);
    });
}

#[test]
fn a_list_reorders_by_moving_elements() {
    each(|h| {
        h.run(
            "var items = signal([1, 2, 3, 4, 5]);
             mount(() -> <div><ul><for {x in items.value}><li>{x}</li></for></ul>
               <button onClick={() -> items.value = [5, 1, 2, 3, 4]}>go</button></div>);",
        );
        let before = h.find("li");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><ul><li>5</li><li>1</li><li>2</li><li>3</li><li>4</li></ul><button>go</button></div>");
        // One move: the five goes first. Nothing is made or removed.
        assert_eq!(h.ops.len(), 1, "{:?}", h.ops);
        let after = h.find("li");
        assert_eq!(after[0], before[4]);
    });
}

#[test]
fn a_map_is_walked_with_its_keys() {
    each(|h| {
        h.run("var m = ['x' => 1, 'y' => 2]; mount(() -> <dl><for {k => v in m}><dt>{k}</dt><dd>{v}</dd></for></dl>);");
        assert_eq!(h.html(), "<dl><dt>x</dt><dd>1</dd><dt>y</dt><dd>2</dd></dl>");
    });
}

#[test]
fn unmounting_leaves_nothing_behind() {
    each(|h| {
        h.run(
            "var n = signal(0);
             var root = mount(() -> <div><if {n.value >= 0}><for {x in [1, 2, 3]}><p>{x}{n.value}</p></for></if></div>);
             return root;",
        );
        assert!(h.vm.view_node_count() > 5);
        let root = h.roots[0];
        assert!(h.vm.view_unmount(root));
        h.apply();
        assert_eq!(h.vm.view_node_count(), 0);
        assert!(h.dom.is_empty(), "{} nodes left in the host", h.dom.len());
    });
}

#[test]
fn a_signal_where_a_value_is_shown_stands_for_its_value() {
    each(|h| {
        h.run(
            "var n = signal(1); var doubled = computed(() -> n.value * 2); var xs = signal([1, 2]); var on = signal(false);
             mount(() -> <div class={on}><p>{n}</p><p>{doubled}</p><for {x in xs}><i>{x}</i></for><if {on}>yes<else>no</if>
               <button onClick={() -> { n.value = 5; xs.value = [1, 2, 3]; on.value = true; }}>go</button></div>);",
        );
        assert_eq!(h.html(), "<div class={false}><p>1</p><p>2</p><i>1</i><i>2</i>no<button>go</button></div>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div class><p>5</p><p>10</p><i>1</i><i>2</i><i>3</i>yes<button>go</button></div>");
    });
}

#[test]
fn content_goes_in_the_right_place_among_its_neighbours() {
    each(|h| {
        h.run(
            "var a = signal(false); var c = signal(false); var xs = signal([]);
             mount(() -> <div><if {a}><i>A</i></if><b>b</b><for {x in xs}><u>{x}</u></for>t<if {c}><s>C</s></if>
               <button onClick={() -> a.value = !a.value}>a</button>
               <button onClick={() -> c.value = !c.value}>c</button>
               <button onClick={() -> xs.value = [1, 2]}>xs</button></div>);",
        );
        let tail = "t<button>a</button><button>c</button><button>xs</button></div>";
        assert_eq!(h.html(), format!("<div><b>b</b>{tail}"));
        h.click_nth("button", 1);
        assert_eq!(h.html(), format!("<div><b>b</b>t<s>C</s><button>a</button><button>c</button><button>xs</button></div>"));
        h.click_nth("button", 0);
        assert_eq!(h.html(), format!("<div><i>A</i><b>b</b>t<s>C</s><button>a</button><button>c</button><button>xs</button></div>"));
        h.click_nth("button", 2);
        assert_eq!(h.html(), format!("<div><i>A</i><b>b</b><u>1</u><u>2</u>t<s>C</s><button>a</button><button>c</button><button>xs</button></div>"));
        h.click_nth("button", 1);
        h.click_nth("button", 0);
        assert_eq!(h.html(), format!("<div><b>b</b><u>1</u><u>2</u>t<button>a</button><button>c</button><button>xs</button></div>"));
    });
}

#[test]
fn an_event_carries_a_payload_to_its_handler() {
    each(|h| {
        h.run("var last = signal('none'); mount(() -> <div><p>{last}</p><input onInput={(e) -> last.value = e}/></div>);");
        let input = h.find("input")[0];
        assert_eq!(h.dom.listens(input), vec!["input".to_string()]);
        let payload = h.vm.new_string("typed");
        assert!(h.vm.view_dispatch(input, "input", payload));
        h.settle();
        assert_eq!(h.html(), "<div><p>typed</p><input/></div>");
        assert!(!h.vm.view_dispatch(input, "keyDown", Value::Nil));
    });
}

#[test]
fn a_handler_may_wait_and_the_view_follows() {
    each(|h| {
        h.run("var n = signal(0); mount(() -> <b onClick={() -> { yield(); yield(); n.value = 7; }}>{n}</b>);");
        h.click_nth("b", 0);
        assert_eq!(h.html(), "<b>7</b>");
    });
}

#[test]
fn a_property_set_to_null_is_taken_away_and_a_handler_too() {
    each(|h| {
        h.run(
            "var t = signal('x'); var f = signal(() -> 1);
             mount(() -> <div><a title={t} onClick={f}>l</a><button onClick={() -> { t.value = null; f.value = null; }}>go</button></div>);",
        );
        let a = h.find("a")[0];
        assert_eq!(h.dom.listens(a), vec!["click".to_string()]);
        assert_eq!(h.html(), "<div><a title=\"x\">l</a><button>go</button></div>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><a>l</a><button>go</button></div>");
        assert!(h.dom.listens(a).is_empty());
    });
}

#[test]
fn attributes_can_be_spread() {
    each(|h| {
        h.run(
            "var p = signal({title: 'a', id: 'x'});
             function Box(props) { return <div {...props}>{props.children}</div>; }
             mount(() -> <div><Box class='k' {...p.value}>in</Box><button onClick={() -> p.value = {title: 'b'}}>go</button></div>);",
        );
        assert_eq!(h.html(), "<div><div class=\"k\" title=\"a\" id=\"x\">in</div><button>go</button></div>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><div class=\"k\" title=\"b\">in</div><button>go</button></div>");
    });
}

#[test]
fn a_component_has_a_scope_that_ends_with_it() {
    each(|h| {
        let log = h.run(
            "var log = [];
             function Timer(props) {
               var n = signal(0);
               effect(() -> log.push('n=' + n.value));
               onCleanup(() -> log.push('gone ' + props.name));
               return <b onClick={() -> n.value++}>{props.name}{n}</b>;
             }
             var show = signal(true);
             mount(() -> <div><if {show}><Timer name='t'/></if><button onClick={() -> show.value = false}>hide</button></div>);
             return log;",
        );
        h.click_nth("b", 0);
        assert_eq!(h.html(), "<div><b>t1</b><button>hide</button></div>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><button>hide</button></div>");
        assert_eq!(h.vm.display(log), "[n=0,n=1,gone t]");
    });
}

#[test]
fn a_component_that_throws_leaves_nothing_mounted() {
    each(|h| {
        let err = h.vm.run_source("t.rays", "function Bad(props) { throw 'broken'; } mount(() -> <div><p>ok</p><Bad/></div>);").unwrap_err();
        assert!(err.contains("broken"), "{err}");
        h.apply();
        assert_eq!(h.vm.view_node_count(), 0);
        assert!(h.dom.is_empty(), "{} nodes left in the host", h.dom.len());
    });
}

#[test]
fn a_handler_that_throws_is_reported_with_where() {
    each(|h| {
        h.run("mount(() -> <b onClick={() -> throw 'boom'}>x</b>);");
        let id = h.find("b")[0];
        assert!(h.vm.view_dispatch(id, "click", Value::Nil));
        h.vm.view_pump(1000);
        let errors = h.vm.view_take_errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("boom") && errors[0].contains("t.rays"), "{errors:?}");
    });
}

#[test]
fn text_changes_kind_in_place() {
    each(|h| {
        h.run(
            "var v = signal(1);
             mount(() -> <div><p>{v}</p><button onClick={() -> v.value = (v.value == 1 ? 'two' : v.value == 'two' ? null : v.value == null ? <i>three</i> : 4)}>go</button></div>);",
        );
        let seen: Vec<String> = (0..4)
            .map(|_| {
                h.click_nth("button", 0);
                h.html()
            })
            .collect();
        assert_eq!(seen, ["<div><p>two</p><button>go</button></div>", "<div><p/><button>go</button></div>", "<div><p><i>three</i></p><button>go</button></div>", "<div><p>4</p><button>go</button></div>"]);
    });
}

#[test]
fn the_same_value_twice_in_a_list_is_two_items() {
    each(|h| {
        h.run(
            "var xs = signal([1, 1, 2]);
             mount(() -> <div><for {x in xs}><i>{x}</i></for><button onClick={() -> xs.value = [2, 1, 1, 1]}>go</button></div>);",
        );
        assert_eq!(h.html(), "<div><i>1</i><i>1</i><i>2</i><button>go</button></div>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><i>2</i><i>1</i><i>1</i><i>1</i><button>go</button></div>");
    });
}

#[test]
fn a_list_in_a_branch_in_a_list() {
    each(|h| {
        h.run(
            "var rows = signal([[1, 2], [], [3]]); var on = signal(true);
             mount(() -> <div><for {r in rows}><if {on}><ul><for {c in r}><li>{c}</li></for></ul><else>off</if></for>
               <button onClick={() -> on.value = !on.value}>t</button></div>);",
        );
        assert_eq!(h.html(), "<div><ul><li>1</li><li>2</li></ul><ul/><ul><li>3</li></ul><button>t</button></div>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div>offoffoff<button>t</button></div>");
        h.click_nth("button", 0);
        assert_eq!(h.html(), "<div><ul><li>1</li><li>2</li></ul><ul/><ul><li>3</li></ul><button>t</button></div>");
    });
}

#[test]
fn two_views_are_separate() {
    each(|h| {
        h.run("var n = signal(0); mount(() -> <p>{n}</p>, 'one'); mount(() -> <p onClick={() -> n.value++}>{n}!</p>, 'two');");
        assert_eq!(h.roots.len(), 2);
        assert_eq!(h.dom.inner(h.roots[0]), "<p>0</p>");
        h.click(h.dom.find(h.roots[1], "p")[0]);
        assert_eq!(h.dom.inner(h.roots[0]), "<p>1</p>");
        assert_eq!(h.dom.inner(h.roots[1]), "<p>1!</p>");
        assert_eq!(h.dom.prop(h.roots[1], "name"), Some(&rayscript_vm::view::PropValue::Str("two".into())));
    });
}

/// Lists made and remade at random come out as the host would draw them from scratch.
#[test]
fn random_list_updates_match_a_fresh_draw() {
    each(|h| {
        let set = h.run("var xs = signal([]); mount(() -> <ul><for {x in xs}><li>{x}</li></for></ul>); return (a) -> xs.value = a;");
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut current: Vec<i64> = Vec::new();
        for round in 0..120 {
            let mut list: Vec<i64> = Vec::new();
            // Keep some, drop some, shuffle some, add some; values repeat now and then.
            for v in &current {
                if next(4) != 0 {
                    list.push(*v);
                }
            }
            for i in (1..list.len()).rev() {
                if next(3) == 0 {
                    list.swap(i, next(i as u64 + 1) as usize);
                }
            }
            for _ in 0..next(4) {
                let at = next(list.len() as u64 + 1) as usize;
                list.insert(at, next(12) as i64);
            }
            let arr = h.vm.new_array(list.iter().map(|v| Value::Int(*v)).collect());
            h.vm.call(set, &[arr]).unwrap();
            h.settle();
            let want: String = list.iter().map(|v| format!("<li>{v}</li>")).collect();
            assert_eq!(h.html(), format!("<ul>{want}</ul>"), "round {round}, from {current:?} to {list:?}");
            current = list;
        }
    });
}

#[test]
fn unmount_cancels_waiting_event_fibers_and_their_children_only() {
    each(|h| {
        let (gate, id) = h.vm.new_promise();
        h.vm.define_global("gate", gate);
        let log = h.run("var log = []; mount(() -> { onCleanup(() -> log.push('cleanup')); return <div><button onClick={() -> { spawn(() -> { await gate; log.push('child'); }); await gate; log.push('first'); }}>one</button></div>; }, 'one'); mount(() -> <button onClick={() -> { await gate; log.push('second'); }}>two</button>, 'two'); return log;");
        let buttons = [h.dom.find(h.roots[0], "button")[0], h.dom.find(h.roots[1], "button")[0]];
        h.click(buttons[0]);
        h.click(buttons[1]);
        assert_eq!(h.vm.fiber_count(), 3);
        assert!(h.vm.view_unmount(h.roots[0]));
        h.settle();
        assert_eq!(h.vm.fiber_count(), 1);
        h.vm.collect();
        assert!(h.vm.resolve(id, Value::Nil));
        h.settle();
        assert_eq!(h.vm.display(log), "[cleanup,second]");
        assert_eq!(h.vm.fiber_count(), 0);
    });
}
