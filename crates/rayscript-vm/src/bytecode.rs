//! The bytecode: a register machine whose instructions are 8 bytes. A function's registers are the
//! first slots of its frame; its parameters are registers 0.. and a call puts the callee in a register
//! with the arguments just above it, so the callee's frame starts at the first argument.

use std::fmt;
use std::rc::Rc;

pub type Reg = u16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Nop,
    Move { dst: Reg, src: Reg },
    LoadNil { dst: Reg },
    LoadBool { dst: Reg, value: bool },
    LoadInt { dst: Reg, value: i32 },
    LoadConst { dst: Reg, index: u32 },

    /// A module-level name, looked up when it runs. `name` indexes the constants (a string).
    GetGlobal { dst: Reg, name: u32 },
    SetGlobal { src: Reg, name: u32 },
    DefGlobal { src: Reg, name: u32 },

    /// A variable a closure captures lives in a cell, held in a register.
    NewCell { dst: Reg, src: Reg },
    GetCell { dst: Reg, cell: Reg },
    SetCell { cell: Reg, src: Reg },
    /// The closure's own captured cells, by index.
    GetUpval { dst: Reg, index: u16 },
    SetUpval { index: u16, src: Reg },
    Closure { dst: Reg, proto: u32 },

    Add { dst: Reg, a: Reg, b: Reg },
    Sub { dst: Reg, a: Reg, b: Reg },
    Mul { dst: Reg, a: Reg, b: Reg },
    Div { dst: Reg, a: Reg, b: Reg },
    Rem { dst: Reg, a: Reg, b: Reg },
    Shl { dst: Reg, a: Reg, b: Reg },
    Shr { dst: Reg, a: Reg, b: Reg },
    UShr { dst: Reg, a: Reg, b: Reg },
    BitAnd { dst: Reg, a: Reg, b: Reg },
    BitOr { dst: Reg, a: Reg, b: Reg },
    BitXor { dst: Reg, a: Reg, b: Reg },
    Neg { dst: Reg, src: Reg },
    Not { dst: Reg, src: Reg },
    BitNot { dst: Reg, src: Reg },
    /// `a + 1` for `++`, with the constant in the instruction.
    AddInt { dst: Reg, src: Reg, value: i16 },

    Eq { dst: Reg, a: Reg, b: Reg },
    NotEq { dst: Reg, a: Reg, b: Reg },
    Lt { dst: Reg, a: Reg, b: Reg },
    Le { dst: Reg, a: Reg, b: Reg },
    Gt { dst: Reg, a: Reg, b: Reg },
    Ge { dst: Reg, a: Reg, b: Reg },

    /// Jumps are relative to the next instruction. One that goes back is a loop edge: the VM polls there.
    Jump { offset: i32 },
    JumpIf { cond: Reg, offset: i32 },
    JumpIfNot { cond: Reg, offset: i32 },
    JumpIfNil { src: Reg, offset: i32 },
    JumpIfNotNil { src: Reg, offset: i32 },

    /// The callee is in `base`, the arguments in the registers above it; the result goes to `base`.
    Call { base: Reg, argc: u16 },
    /// A method call: the receiver is in `base`, the arguments above it, the name a string constant.
    CallMethod { base: Reg, argc: u16, name: u32 },
    Return { src: Reg },
    ReturnNil,

    GetField { dst: Reg, obj: Reg, name: u32 },
    SetField { obj: Reg, name: u32, src: Reg },
    GetIndex { dst: Reg, obj: Reg, index: Reg },
    SetIndex { obj: Reg, index: Reg, src: Reg },
    NewArray { dst: Reg, capacity: u16 },
    ArrayPush { array: Reg, src: Reg },
    NewObject { dst: Reg },
    NewMap { dst: Reg },
    /// `a...b`
    NewRange { dst: Reg, from: Reg, to: Reg },

    /// Concatenates `count` registers from `base` as strings into `dst`.
    Interp { dst: Reg, base: Reg, count: u16 },

    /// Begins a loop over `src`: `base` takes the iterable and `base + 1` its position.
    IterInit { base: Reg, src: Reg },
    /// The next element into `value` (and the key or index into `key`); jumps `offset` when there is none.
    IterNext { base: Reg, key: Reg, value: Reg, offset: i32 },
    /// Integer loops `a...b`: `base` holds the next, `base + 1` the end. Jumps when done.
    ForRange { base: Reg, var: Reg, offset: i32 },

    Throw { src: Reg },
    /// Starts a `try`: an error thrown until `TryEnd` lands at `catch_offset` with the error in `dst`.
    TryBegin { dst: Reg, catch_offset: i32 },
    TryEnd,
    /// `dst = <the value of src is of the type named by the string constant>`, for typed catches.
    IsType { dst: Reg, src: Reg, name: u32 },
    /// Throws the error again, for a `try` whose catches none matched.
    Rethrow { src: Reg },

    /// The elements of the array `src` from position `from`, as a new array, for `...rest` in a pattern.
    Slice { dst: Reg, src: Reg, from: u16 },
    /// A member of a host module by its dotted path (a string constant), for `import`.
    Import { dst: Reg, path: u32 },
    /// Every member of a host module as a global, for `import a.b.*`.
    ImportAll { path: u32 },
    /// Binds a function (`library\0name\0signature\0flags`) or handle type
    /// (`library\0name\0handle\0free_symbol`) from a string constant.
    BindNative { dst: Reg, spec: u32 },

    /// Pattern tests: whether `src` is an array of exactly (or at least) `len` elements, an object, or has a field.
    TestArray { dst: Reg, src: Reg, len: u16, at_least: bool },
    TestObject { dst: Reg, src: Reg },
    TestField { dst: Reg, src: Reg, name: u32 },

    /// Suspends the fiber until `src` (a fiber or a host request) is done; its value goes to `dst`.
    Await { dst: Reg, src: Reg },

    /// Put in place of an instruction by the debugger, which keeps the original: where a fiber may stop.
    Break,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Int(i64),
    Float(f64),
    Str(Rc<str>),
}

/// Where a closure's captured cell comes from when it is made: a cell in a register of the
/// enclosing function, or one of the enclosing closure's own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UpvalDesc {
    pub from_parent_local: bool,
    pub index: u16,
}

/// A named variable of a function, for a debugger: the register it lives in (holding a cell when a closure captures
/// it) and the instructions, from `start` up to `end`, it is in scope for.
#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: Rc<str>,
    pub reg: Reg,
    pub cell: bool,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug)]
pub struct Proto {
    pub name: Rc<str>,
    /// Parameters, counting a rest parameter.
    pub params: u16,
    pub has_rest: bool,
    /// Registers the frame needs.
    pub registers: u16,
    pub code: Vec<Op>,
    /// The source byte offset each instruction came from, for errors and traces.
    pub offsets: Vec<u32>,
    pub consts: Vec<Const>,
    pub upvals: Vec<UpvalDesc>,
    /// The function's named variables, in the order they were declared.
    pub locals: Vec<LocalInfo>,
}

/// A compiled script: its functions, with `main` the one that runs the module's own statements.
#[derive(Clone, Debug)]
pub struct Module {
    pub name: String,
    pub source: Rc<str>,
    pub protos: Vec<Proto>,
    pub main: u32,
}

impl fmt::Display for Const {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Const::Int(v) => write!(f, "{v}"),
            Const::Float(v) => write!(f, "{v:?}"),
            Const::Str(s) => write!(f, "{s:?}"),
        }
    }
}

/// The listing of a function, for tests and `rayscript dump`.
pub fn disassemble(p: &Proto) -> String {
    let mut out = format!("function {} (params {}{}, registers {})\n", p.name, p.params, if p.has_rest { ", rest" } else { "" }, p.registers);
    for (i, op) in p.code.iter().enumerate() {
        out.push_str(&format!("  {i:4}  {op:?}\n"));
    }
    for (i, c) in p.consts.iter().enumerate() {
        out.push_str(&format!("  k{i} = {c}\n"));
    }
    out
}

pub fn disassemble_module(m: &Module) -> String {
    m.protos.iter().enumerate().map(|(i, p)| format!("[{i}] {}", disassemble(p))).collect::<Vec<_>>().join("\n")
}
