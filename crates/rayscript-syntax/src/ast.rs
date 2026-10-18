//! The syntax tree. Every node carries its span, so tools can point at exactly what they mean.
//! Types, metadata and modifiers are parsed and kept for tooling; the runtime does not enforce them.

use crate::span::Span;

#[derive(Clone, Debug)]
pub struct Module {
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Item {
    Import(Import),
    Native(NativeBlock),
    /// A statement: a declaration or an expression.
    Stmt(Expr),
}

/// `import a.b.C;`, `import a.b.C as D;` or `import a.b.*;`.
#[derive(Clone, Debug)]
pub struct Import {
    pub path: Vec<Ident>,
    pub alias: Option<Ident>,
    pub wildcard: bool,
    pub span: Span,
}

/// `native "library" { function name(a:Int, b:String):Int; ... }`: functions of a native library.
#[derive(Clone, Debug)]
pub struct NativeBlock {
    pub library: String,
    pub library_span: Span,
    pub functions: Vec<NativeFn>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct NativeFn {
    pub meta: Vec<Meta>,
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Meta {
    /// The name, with its leading colon when it has one: `:blocking`, `deprecated`.
    pub name: String,
    pub args: Vec<Expr>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Public,
    Private,
    Static,
    Inline,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Expr {
        Expr { kind, span }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Coalesce,
    /// `a...b`
    Interval,
}

#[derive(Clone, Debug)]
pub enum InterpPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A single-quoted string with `$name` and `${expr}` in it.
    Interp(Vec<InterpPart>),
    Ident(String),
    Paren(Box<Expr>),
    Array(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Object(Vec<ObjField>),
    /// `[for (x in xs) expr]` or `["k" => v for ...]`: the body is a `for` or `while` whose value is collected.
    Comprehension { map: bool, body: Box<Expr> },
    Block(Vec<Expr>),
    Var(Box<VarDecls>),
    Function(Box<FnDecl>),
    Lambda(Box<Lambda>),
    Field { obj: Box<Expr>, name: Ident, safe: bool },
    Index { obj: Box<Expr>, index: Box<Expr> },
    Call { callee: Box<Expr>, args: Vec<Expr> },
    New { path: Vec<Ident>, args: Vec<Expr> },
    Unary { op: UnOp, expr: Box<Expr> },
    Binary { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    /// `a = b`, or `a += b` with the operator.
    Assign { op: Option<BinOp>, target: Box<Expr>, value: Box<Expr> },
    Ternary { cond: Box<Expr>, then: Box<Expr>, els: Box<Expr> },
    If { cond: Box<Expr>, then: Box<Expr>, els: Option<Box<Expr>> },
    While { cond: Box<Expr>, body: Box<Expr> },
    DoWhile { body: Box<Expr>, cond: Box<Expr> },
    For { key: Option<Ident>, var: Ident, iter: Box<Expr>, body: Box<Expr> },
    Return(Option<Box<Expr>>),
    Break,
    Continue,
    Throw(Box<Expr>),
    Try { body: Box<Expr>, catches: Vec<Catch> },
    Switch { subject: Box<Expr>, cases: Vec<Case>, default: Option<Box<Expr>> },
    Await(Box<Expr>),
    Markup(Box<MarkupNode>),
    /// Where the parser found something it could not read; a diagnostic was reported.
    Error,
}

#[derive(Clone, Debug)]
pub struct ObjField {
    pub name: String,
    pub name_span: Span,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub struct VarDecls {
    pub is_final: bool,
    pub modifiers: Vec<Modifier>,
    pub meta: Vec<Meta>,
    pub decls: Vec<VarDecl>,
}

#[derive(Clone, Debug)]
pub struct VarDecl {
    pub name: Ident,
    pub ty: Option<Type>,
    pub init: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Ident,
    pub ty: Option<Type>,
    pub default: Option<Expr>,
    /// `?name`
    pub optional: bool,
    /// `...name`
    pub rest: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub name: Option<Ident>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Expr,
    pub modifiers: Vec<Modifier>,
    pub meta: Vec<Meta>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Lambda {
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Catch {
    pub name: Ident,
    pub ty: Option<Type>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Case {
    /// The patterns, any of which makes the case (`case 1, 2:` and `case A | B:`).
    pub patterns: Vec<Pattern>,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum PatternKind {
    Wildcard,
    /// A name that takes the value.
    Bind(String),
    /// A literal the value must equal.
    Lit(Box<Expr>),
    /// `[a, b, ...rest]`
    Array { items: Vec<Pattern>, rest: Option<Option<Ident>> },
    /// `{x: 1, y: p}`
    Object(Vec<(Ident, Pattern)>),
    /// `Some(x)`, `ui.Kind.Button`
    Ctor { path: Vec<Ident>, args: Vec<Pattern> },
}

#[derive(Clone, Debug)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    Path { path: Vec<Ident>, params: Vec<Type> },
    Func { args: Vec<Type>, ret: Box<Type> },
    Anon(Vec<TypeField>),
    Optional(Box<Type>),
}

#[derive(Clone, Debug)]
pub struct TypeField {
    pub name: Ident,
    pub ty: Type,
    pub optional: bool,
}

#[derive(Clone, Debug)]
pub enum MarkupNode {
    Element(Element),
    /// `<>...</>`
    Fragment { children: Vec<MarkupNode>, span: Span },
    Text { text: String, span: Span },
    /// `{expr}` among children.
    Hole { expr: Expr, span: Span },
    /// `<if {cond}>...<else>...</if>`
    If { cond: Expr, then: Vec<MarkupNode>, els: Option<Vec<MarkupNode>>, span: Span },
    /// `<for {x in xs}>...</for>`, or `{k => v in map}`.
    For { key: Option<Ident>, var: Ident, iter: Expr, body: Vec<MarkupNode>, span: Span },
}

impl MarkupNode {
    pub fn span(&self) -> Span {
        match self {
            MarkupNode::Element(e) => e.span,
            MarkupNode::Fragment { span, .. }
            | MarkupNode::Text { span, .. }
            | MarkupNode::Hole { span, .. }
            | MarkupNode::If { span, .. }
            | MarkupNode::For { span, .. } => *span,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Element {
    /// The tag as written: `div`, `tab-list`, `ui.my-card`, `Counter`.
    pub tag: Ident,
    pub attrs: Vec<Attr>,
    pub children: Vec<MarkupNode>,
    pub self_closing: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Attr {
    Named { name: Ident, value: AttrValue, span: Span },
    /// `{...expr}`
    Spread { expr: Expr, span: Span },
}

#[derive(Clone, Debug)]
pub enum AttrValue {
    /// `<input disabled>`
    Flag,
    Str(String),
    Interp(Vec<InterpPart>),
    Expr(Expr),
}
