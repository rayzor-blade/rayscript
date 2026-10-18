//! What a debugger needs of the VM: breakpoints on lines, pausing a fiber and stepping it, its frames and
//! their variables.
//!
//! A fiber stops where the debugger has put a `Break` in place of an instruction, keeping the instruction it
//! replaced. The interpreter pays nothing for it: `Break` is one more instruction, and a script that is not
//! being debugged has none. A `Break` that is reached and does not stop the fiber is taken out so the instruction
//! runs, and put back at the next poll, which a second visit to the same instruction cannot come before.
//!
//! A fiber stops at an instruction that has not run: its saved `pc` is that instruction, and resuming runs it. While
//! one is stopped the VM runs nothing else: `tick` and `run_until_idle` do nothing until `debug_resume`. A call a
//! built-in made back into script (`array.map(f)`) cannot be stopped in.

use super::{ops, Vm};
use crate::bytecode::Op;
use crate::object::Kind;
use crate::value::{Obj, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

/// How a resumed fiber goes on until it stops again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepMode {
    /// To the next line of this function, or out of it, not into the calls on the way.
    Over,
    /// To the next line, going into a call that starts on the way.
    In,
    /// Until this function has returned.
    Out,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PauseReason {
    Breakpoint,
    Step,
    /// Asked for with `debug_pause`.
    Request,
}

#[derive(Clone, Copy)]
struct Step {
    fiber: Obj,
    mode: StepMode,
    depth: usize,
    line: u32,
}

pub(crate) struct Pause {
    pub fiber: Obj,
    pub reason: PauseReason,
}

/// A call in a paused fiber's stack, as the debugger lists it.
#[derive(Clone, Debug)]
pub struct DebugFrame {
    pub function: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// A variable or a member of one: its name, its value as text, its type, and a number to expand it by (0: nothing inside).
#[derive(Clone, Debug)]
pub struct DebugVar {
    pub name: String,
    pub value: String,
    pub type_name: &'static str,
    pub reference: u32,
}

#[derive(Default)]
pub struct Debugger {
    /// Whether the `Break`s in place are not what they should be: a poll puts them right.
    pub(crate) dirty: bool,
    /// The lines asked for, by the name of the module's file.
    wanted: HashMap<String, BTreeSet<u32>>,
    /// The instructions those lines start at, as (function, pc).
    stops: HashSet<(u32, u32)>,
    step: Option<Step>,
    pause_requested: bool,
    /// The `Break`s in place, and the instruction each replaced.
    patched: HashMap<(u32, u32), Op>,
    /// The instruction a resumed fiber starts at, which is not put a `Break` on until the fiber has gone on.
    hold: Option<(u32, u32)>,
    pub(crate) paused: Option<Pause>,
    /// The values the debugger has been given a number for, alive while paused.
    pub(crate) refs: Vec<Value>,
}

impl Debugger {
    pub(crate) fn wanted_is_empty(&self) -> bool {
        self.wanted.is_empty() && self.patched.is_empty()
    }
}

const MAX_CHILDREN: usize = 200;

impl Vm {
    /// The line (from 1) of a source offset in a module.
    fn line_at(&self, module: u32, offset: u32) -> u32 {
        self.locate(module, offset).1
    }

    /// The first instruction of each run on one line in a function, with the line.
    fn line_starts(&self, proto: usize) -> Vec<(u32, u32)> {
        let p = &self.protos[proto];
        let mut out = Vec::new();
        let mut before = 0;
        for pc in 0..p.code.len() {
            let line = self.line_at(p.module, p.offsets.get(pc).copied().unwrap_or(0));
            if pc == 0 || line != before {
                out.push((line, pc as u32));
            }
            before = line;
        }
        out
    }

    /// Puts the breakpoints of the file named `file` on `lines`, replacing the ones it had; the lines they are
    /// on, each moved down to the next line that has code, and none for a line past the last. Breakpoints
    /// asked for before a module is loaded take hold when it is.
    pub fn debug_set_breakpoints(&mut self, file: &str, lines: &[u32]) -> Vec<u32> {
        if lines.is_empty() {
            self.debug.wanted.remove(file);
        } else {
            self.debug.wanted.insert(file.to_string(), lines.iter().copied().collect());
        }
        let resolved = self.debug_resolve();
        lines.iter().filter_map(|l| resolved.get(file).and_then(|m| m.get(l)).copied()).collect()
    }

    /// Finds the instructions each wanted line starts at in every loaded module; the line each lands on.
    pub(crate) fn debug_resolve(&mut self) -> HashMap<String, HashMap<u32, u32>> {
        let mut stops = HashSet::new();
        let mut landed: HashMap<String, HashMap<u32, u32>> = HashMap::new();
        for (m, module) in self.modules.iter().enumerate() {
            let Some(wanted) = self.debug.wanted.get(&module.name) else { continue };
            let mut starts: Vec<(u32, u32, u32)> = Vec::new();
            for (pi, p) in self.protos.iter().enumerate() {
                if p.module == m as u32 {
                    starts.extend(self.line_starts(pi).into_iter().map(|(line, pc)| (line, pi as u32, pc)));
                }
            }
            let code: BTreeSet<u32> = starts.iter().map(|s| s.0).collect();
            let entry = landed.entry(module.name.clone()).or_default();
            for want in wanted {
                let Some(line) = code.range(*want..).next().copied() else { continue };
                entry.insert(*want, line);
                for (l, pi, pc) in &starts {
                    if *l == line {
                        stops.insert((*pi, *pc));
                    }
                }
            }
        }
        self.debug.stops = stops;
        self.debug_sync();
        landed
    }

    /// The instructions that should hold a `Break` now: the breakpoints; while stepping, where a step may end; and,
    /// for a pause asked for, every instruction.
    fn debug_wanted(&self) -> HashSet<(u32, u32)> {
        let mut want = self.debug.stops.clone();
        if self.debug.pause_requested {
            for (pi, p) in self.protos.iter().enumerate() {
                for pc in 0..p.code.len() {
                    want.insert((pi as u32, pc as u32));
                }
            }
        }
        if let Some(step) = self.debug.step {
            let frames: Vec<usize> = step.fiber.fiber().fiber.frames.iter().map(|f| f.closure.closure().proto as usize).collect();
            for pi in frames {
                for (_, pc) in self.line_starts(pi) {
                    want.insert((pi as u32, pc));
                }
            }
            if step.mode == StepMode::In {
                for pi in 0..self.protos.len() {
                    want.insert((pi as u32, 0));
                }
            }
        }
        want
    }

    /// Makes the `Break`s in place the ones wanted, the instruction a resumed fiber started at included.
    pub(crate) fn debug_sync(&mut self) {
        self.debug.hold = None;
        self.debug_apply();
    }

    /// Takes out the `Break`s that are no longer wanted and puts in those that are missing, but for the held instruction.
    fn debug_apply(&mut self) {
        self.debug.dirty = false;
        let want = self.debug_wanted();
        let stale: Vec<(u32, u32)> = self.debug.patched.keys().filter(|k| !want.contains(k)).copied().collect();
        for k in stale {
            if let Some(op) = self.debug.patched.remove(&k) {
                self.protos[k.0 as usize].code[k.1 as usize] = op;
            }
        }
        for k in want {
            if self.debug.patched.contains_key(&k) || self.debug.hold == Some(k) {
                continue;
            }
            let Some(code) = self.protos.get_mut(k.0 as usize).map(|p| &mut p.code) else { continue };
            let Some(op) = code.get(k.1 as usize).copied() else { continue };
            if op != Op::Break {
                self.debug.patched.insert(k, op);
                code[k.1 as usize] = Op::Break;
            }
        }
    }

    /// Whether a module with this name is loaded, so a breakpoint in it can be placed.
    pub fn debug_module_loaded(&self, name: &str) -> bool {
        self.modules.iter().any(|m| m.name == name)
    }

    /// Stops the running fiber at its next instruction.
    pub fn debug_pause(&mut self) {
        self.debug.pause_requested = true;
        self.debug_sync();
    }

    /// Whether a fiber is stopped, and why.
    pub fn debug_paused(&self) -> Option<PauseReason> {
        self.debug.paused.as_ref().map(|p| p.reason)
    }

    /// A fiber reached a `Break`: takes it out so the instruction can run, and says whether the fiber stops here.
    pub(crate) fn debug_hit(&mut self, fo: Obj, frame: usize, pc: usize, proto: usize) -> Option<PauseReason> {
        let key = (proto as u32, pc as u32);
        match self.debug.patched.remove(&key) {
            Some(op) => self.protos[proto].code[pc] = op,
            // Not one the debugger put there: nothing to run, so it is a no-op.
            None => self.protos[proto].code[pc] = Op::Nop,
        }
        self.debug.dirty = true;
        if self.debug.hold == Some(key) {
            self.debug.hold = None;
            return None;
        }
        if self.nested > 0 {
            return None;
        }
        let _ = frame;
        let reason = if self.debug.pause_requested {
            self.debug.pause_requested = false;
            Some(PauseReason::Request)
        } else if self.debug.stops.contains(&key) {
            Some(PauseReason::Breakpoint)
        } else if let Some(step) = self.debug.step.filter(|s| s.fiber == fo) {
            let depth = fo.fiber().fiber.frames.len();
            let lp = &self.protos[proto];
            let line = self.line_at(lp.module, lp.offsets.get(pc).copied().unwrap_or(0));
            let starts = pc == 0 || self.line_at(lp.module, lp.offsets.get(pc - 1).copied().unwrap_or(0)) != line;
            let stop = match step.mode {
                StepMode::Out => depth < step.depth,
                StepMode::Over => depth < step.depth || (depth == step.depth && starts && line != step.line),
                StepMode::In => depth != step.depth && (depth < step.depth || pc == 0) || (depth == step.depth && starts && line != step.line),
            };
            stop.then_some(PauseReason::Step)
        } else {
            None
        };
        if reason.is_some() {
            self.debug.hold = Some(key);
        }
        reason
    }

    /// Notes that a fiber stopped; called as it leaves the interpreter.
    pub(crate) fn debug_stopped(&mut self, fiber: Obj, reason: PauseReason) {
        self.debug.step = None;
        self.debug.refs.clear();
        self.debug.paused = Some(Pause { fiber, reason });
        self.debug.dirty = true;
    }

    /// Lets the stopped fiber go on: to its next breakpoint, or as `mode` says. The fiber runs when the VM next does.
    pub fn debug_resume(&mut self, mode: Option<StepMode>) -> bool {
        let Some(p) = self.debug.paused.take() else { return false };
        let (depth, line, at) = {
            let f = &p.fiber.fiber().fiber;
            let fr = f.frames[f.frames.len() - 1];
            let proto = &self.protos[fr.closure.closure().proto as usize];
            (f.frames.len(), self.line_at(proto.module, proto.offsets.get(fr.pc).copied().unwrap_or(0)), (fr.closure.closure().proto, fr.pc as u32))
        };
        self.debug.step = mode.map(|mode| Step { fiber: p.fiber, mode, depth, line });
        self.debug.refs.clear();
        self.ready.push_front(p.fiber);
        self.debug.hold = Some(at);
        self.debug_apply();
        // The instruction it starts at is put a `Break` on again at the first poll after it has run.
        self.debug.dirty = true;
        true
    }

    /// The calls of the stopped fiber, the innermost first.
    pub fn debug_frames(&self) -> Vec<DebugFrame> {
        let Some(p) = &self.debug.paused else { return Vec::new() };
        let f = &p.fiber.fiber().fiber;
        let last = f.frames.len().saturating_sub(1);
        f.frames
            .iter()
            .enumerate()
            .rev()
            .map(|(i, fr)| {
                let proto = &self.protos[fr.closure.closure().proto as usize];
                let at = if i == last { fr.pc } else { fr.pc.saturating_sub(1) };
                let (file, line, col) = self.locate(proto.module, proto.offsets.get(at).copied().unwrap_or(0));
                DebugFrame { function: proto.name.to_string(), file, line, col }
            })
            .collect()
    }

    /// The frame `index` of the stopped fiber (0 is the innermost), and the instruction it is at.
    fn debug_frame(&self, index: usize) -> Option<(crate::fiber::Frame, usize, Obj)> {
        let p = self.debug.paused.as_ref()?;
        let f = &p.fiber.fiber().fiber;
        let at = f.frames.len().checked_sub(1 + index)?;
        let fr = f.frames[at];
        let pc = if index == 0 { fr.pc } else { fr.pc.saturating_sub(1) };
        Some((fr, pc, p.fiber))
    }

    fn debug_var(&mut self, name: &str, v: Value) -> DebugVar {
        let expandable = match v {
            Value::Obj(o) => match o.kind() {
                Kind::Array => !o.array().items.is_empty(),
                Kind::Object => o.object().fields.len() > 0,
                Kind::Map => o.map().map.len() > 0,
                Kind::Signal => true,
                _ => false,
            },
            _ => false,
        };
        let reference = if expandable {
            self.debug.refs.push(v);
            self.debug.refs.len() as u32
        } else {
            0
        };
        DebugVar { name: name.to_string(), value: ops::display(self, v, 0), type_name: ops::type_name(v), reference }
    }

    /// The named variables in scope at frame `index` of the stopped fiber, the innermost declaration of a name only.
    pub fn debug_locals(&mut self, index: usize) -> Vec<DebugVar> {
        let Some((fr, pc, fo)) = self.debug_frame(index) else { return Vec::new() };
        let proto = &self.protos[fr.closure.closure().proto as usize];
        let mut found: Vec<(String, Value)> = Vec::new();
        for info in proto.locals.iter() {
            if (info.start as usize) <= pc && pc < info.end as usize {
                let mut v = fo.fiber().fiber.stack.get(fr.base + info.reg as usize).copied().unwrap_or(Value::Nil);
                if info.cell {
                    if let Value::Obj(c) = v {
                        if c.kind() == Kind::Cell {
                            v = c.cell().value;
                        }
                    }
                }
                found.retain(|(n, _)| **n != *info.name);
                found.push((info.name.to_string(), v));
            }
        }
        found.into_iter().map(|(n, v)| self.debug_var(&n, v)).collect()
    }

    /// The module-level variables and functions of the module frame `index` is in, by name.
    pub fn debug_globals(&mut self, index: usize) -> Vec<DebugVar> {
        let Some((fr, _, _)) = self.debug_frame(index) else { return Vec::new() };
        let module = self.protos[fr.closure.closure().proto as usize].module as usize;
        let mut items: Vec<(String, Value)> = self.modules[module].globals.iter().map(|(s, v)| (self.syms.name(*s).to_string(), *v)).collect();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        items.into_iter().map(|(n, v)| self.debug_var(&n, v)).collect()
    }

    /// What is inside the value a variable was given the number `reference` for.
    pub fn debug_expand(&mut self, reference: u32) -> Vec<DebugVar> {
        let Some(v) = (reference as usize).checked_sub(1).and_then(|i| self.debug.refs.get(i).copied()) else { return Vec::new() };
        let Value::Obj(o) = v else { return Vec::new() };
        let items: Vec<(String, Value)> = match o.kind() {
            Kind::Array => o.array().items.iter().take(MAX_CHILDREN).enumerate().map(|(i, x)| (i.to_string(), *x)).collect(),
            Kind::Object => o.object().fields.iter().take(MAX_CHILDREN).map(|(s, x)| (self.syms.name(*s).to_string(), *x)).collect(),
            Kind::Map => o.map().map.iter().take(MAX_CHILDREN).map(|(k, x)| (ops::display(self, k, 0), x)).collect(),
            Kind::Signal => vec![("value".to_string(), o.signal().value)],
            _ => Vec::new(),
        };
        items.into_iter().map(|(n, x)| self.debug_var(&n, x)).collect()
    }

    /// The value of a path of names and members in frame `index`: `todo.items[2].title`. A name is a variable of
    /// the frame, then of its module, then a built-in; a member is a field, an index or the `value` of a signal.
    pub fn debug_evaluate(&mut self, index: usize, expr: &str) -> Result<DebugVar, String> {
        let text = expr.trim();
        if text.is_empty() {
            return Err("nothing to evaluate".into());
        }
        let (name, mut rest) = {
            let end = text.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(text.len());
            (&text[..end], &text[end..])
        };
        if name.is_empty() || name.chars().next().unwrap().is_ascii_digit() {
            return Err(format!("`{text}` is not a variable or a path of members"));
        }
        let mut value = self.debug_lookup(index, name).ok_or_else(|| format!("`{name}` is not defined here"))?;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('.') {
                let end = after.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(after.len());
                let member = &after[..end];
                value = self.debug_member(value, member).ok_or_else(|| format!("{} has no `{member}`", ops::type_name(value)))?;
                rest = &after[end..];
            } else if let Some(after) = rest.strip_prefix('[') {
                let close = after.find(']').ok_or("missing `]`")?;
                let key = after[..close].trim().trim_matches(|c| c == '"' || c == '\'');
                value = self.debug_member(value, key).ok_or_else(|| format!("{} has no `{key}`", ops::type_name(value)))?;
                rest = &after[close + 1..];
            } else {
                return Err(format!("cannot evaluate `{rest}`: only names, members and indexes"));
            }
        }
        Ok(self.debug_var(text, value))
    }

    fn debug_lookup(&mut self, index: usize, name: &str) -> Option<Value> {
        let (fr, pc, fo) = self.debug_frame(index)?;
        let proto = &self.protos[fr.closure.closure().proto as usize];
        let mut local = None;
        for info in proto.locals.iter() {
            if &*info.name == name && (info.start as usize) <= pc && pc < info.end as usize {
                let mut v = fo.fiber().fiber.stack.get(fr.base + info.reg as usize).copied().unwrap_or(Value::Nil);
                if info.cell {
                    if let Value::Obj(c) = v {
                        if c.kind() == Kind::Cell {
                            v = c.cell().value;
                        }
                    }
                }
                local = Some(v);
            }
        }
        if local.is_some() {
            return local;
        }
        let sym = self.syms.lookup(name)?;
        self.modules[proto.module as usize].globals.get(&sym).copied().or_else(|| self.globals.get(&sym).copied())
    }

    fn debug_member(&mut self, v: Value, key: &str) -> Option<Value> {
        let Value::Obj(o) = v else { return None };
        match o.kind() {
            Kind::Object => self.syms.lookup(key).and_then(|s| o.object().fields.get(s)),
            Kind::Array => key.parse::<usize>().ok().and_then(|i| o.array().items.get(i).copied()),
            Kind::Signal if key == "value" => Some(o.signal().value),
            Kind::Map => o.map().map.iter().find(|(k, _)| ops::display(self, *k, 0) == key).map(|(_, x)| x),
            _ => None,
        }
    }
}
