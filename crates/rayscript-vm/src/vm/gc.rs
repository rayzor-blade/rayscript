//! Collection: mark from every root the VM knows, then sweep. A collection happens only at a poll of a
//! running fiber, or when the host asks. Native call frames trace values retained across callbacks.

use super::Vm;
use crate::object::{Kind, drop_object, trace_children};
use crate::value::{Obj, Value};

impl Vm {
    /// Frees every object nothing reachable refers to.
    pub fn collect(&mut self) {
        self.heap.begin_collect();
        let mut gray: Vec<Obj> = Vec::new();
        fn mark(heap: &mut crate::heap::Heap, gray: &mut Vec<Obj>, v: Value) {
            if let Value::Obj(o) = v {
                if unsafe { heap.mark(o.as_ptr()) } {
                    gray.push(o);
                }
            }
        }
        let mut roots: Vec<Value> = Vec::new();
        roots.extend(self.globals.values().copied());
        roots.extend(self.host_modules.values().copied());
        roots.extend(self.script_modules.values().flatten().copied());
        for m in &self.modules {
            roots.extend(m.globals.values().copied());
        }
        roots.extend(self.roots.iter().copied());
        roots.extend(self.debug.refs.iter().copied());
        for p in &self.protos {
            roots.extend(p.consts.iter().copied());
        }
        for f in &self.fibers {
            roots.push(Value::Obj(*f));
        }
        for f in &self.ready {
            roots.push(Value::Obj(*f));
        }
        if let Some(c) = self.current {
            roots.push(Value::Obj(c));
        }
        for p in self.pending.values() {
            roots.push(Value::Obj(*p));
        }
        self.view.trace(&mut |v| roots.push(v));
        self.foreign.trace(&mut |v| roots.push(v));
        for v in roots {
            mark(&mut self.heap, &mut gray, v);
        }
        while let Some(o) = gray.pop() {
            let heap = &mut self.heap;
            trace_children(o, &mut |v| mark(heap, &mut gray, v));
        }
        // Registers above a fiber's live ones may hold anything the last calls left; clear them.
        for f in &self.fibers {
            if f.kind() == Kind::Fiber {
                let fib = &mut f.fiber().fiber;
                let top = fib.top.min(fib.stack.len());
                for v in &mut fib.stack[top..] {
                    *v = Value::Nil;
                }
            }
        }
        self.heap.end_collect(|h| unsafe { drop_object(h) });
        self.foreign.drain();
        self.sync_native_storage();
        self.collections += 1;
    }

    /// How many collections have run.
    pub fn collections(&self) -> usize {
        self.collections
    }

    pub fn heap_stats(&self) -> crate::heap::HeapStats {
        self.heap.stats()
    }
}

impl Drop for Vm {
    /// An empty collection frees every object and runs the destructors of those that own Rust memory.
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.shutdown_native();
            self.heap.begin_collect();
            self.heap.end_collect(|h| unsafe { drop_object(h) });
            self.foreign.drain();
        }
    }
}
