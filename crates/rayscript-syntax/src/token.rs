//! Tokens.

use crate::span::Span;

/// A piece of a single-quoted string: text, `$name`, or `${expression}` (the span of what is inside the braces).
#[derive(Clone, PartialEq, Debug)]
pub enum StrPart {
    Lit(String),
    Ident(String, Span),
    Expr(Span),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kw {
    Var,
    Final,
    Function,
    Return,
    If,
    Else,
    For,
    While,
    Do,
    Break,
    Continue,
    Switch,
    Case,
    Default,
    Try,
    Catch,
    Throw,
    New,
    In,
    True,
    False,
    Null,
    Import,
    Await,
    Native,
    Public,
    Private,
    Static,
    Inline,
}

impl Kw {
    pub fn from_str(s: &str) -> Option<Kw> {
        Some(match s {
            "var" => Kw::Var,
            "final" => Kw::Final,
            "function" => Kw::Function,
            "return" => Kw::Return,
            "if" => Kw::If,
            "else" => Kw::Else,
            "for" => Kw::For,
            "while" => Kw::While,
            "do" => Kw::Do,
            "break" => Kw::Break,
            "continue" => Kw::Continue,
            "switch" => Kw::Switch,
            "case" => Kw::Case,
            "default" => Kw::Default,
            "try" => Kw::Try,
            "catch" => Kw::Catch,
            "throw" => Kw::Throw,
            "new" => Kw::New,
            "in" => Kw::In,
            "true" => Kw::True,
            "false" => Kw::False,
            "null" => Kw::Null,
            "import" => Kw::Import,
            "await" => Kw::Await,
            "native" => Kw::Native,
            "public" => Kw::Public,
            "private" => Kw::Private,
            "static" => Kw::Static,
            "inline" => Kw::Inline,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kw::Var => "var",
            Kw::Final => "final",
            Kw::Function => "function",
            Kw::Return => "return",
            Kw::If => "if",
            Kw::Else => "else",
            Kw::For => "for",
            Kw::While => "while",
            Kw::Do => "do",
            Kw::Break => "break",
            Kw::Continue => "continue",
            Kw::Switch => "switch",
            Kw::Case => "case",
            Kw::Default => "default",
            Kw::Try => "try",
            Kw::Catch => "catch",
            Kw::Throw => "throw",
            Kw::New => "new",
            Kw::In => "in",
            Kw::True => "true",
            Kw::False => "false",
            Kw::Null => "null",
            Kw::Import => "import",
            Kw::Await => "await",
            Kw::Native => "native",
            Kw::Public => "public",
            Kw::Private => "private",
            Kw::Static => "static",
            Kw::Inline => "inline",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum TokenKind {
    Ident(String),
    Kw(Kw),
    Int(i64),
    Float(f64),
    /// A string with no interpolation, escapes resolved.
    Str(String),
    /// A single-quoted string with `$` in it.
    Interp(Vec<StrPart>),

    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Semi,
    Colon,
    Dot,
    /// `...`, the interval operator and the rest-parameter mark.
    Ellipsis,
    Question,
    /// `?.`
    SafeDot,
    At,
    /// `=>`
    FatArrow,
    /// `->`
    Arrow,

    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Bang,
    Shl,
    Shr,
    UShr,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    NotEq,
    AmpAmp,
    PipePipe,
    /// `??`
    Coalesce,
    PlusPlus,
    MinusMinus,

    Assign,
    /// A compound assignment: the operator without the `=`.
    AssignOp(AssignOp),

    Eof,
    /// Something the lexer could not read; a diagnostic was reported for it.
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssignOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    UShr,
    Coalesce,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// Whether a newline lies between the previous token and this one.
    pub newline_before: bool,
}

impl TokenKind {
    /// How the token is named in a message.
    pub fn describe(&self) -> String {
        match self {
            TokenKind::Ident(s) => format!("`{s}`"),
            TokenKind::Kw(k) => format!("`{}`", k.as_str()),
            TokenKind::Int(_) | TokenKind::Float(_) => "a number".into(),
            TokenKind::Str(_) | TokenKind::Interp(_) => "a string".into(),
            TokenKind::Eof => "the end of the script".into(),
            TokenKind::Error => "an unreadable character".into(),
            other => format!("`{}`", other.symbol()),
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::LBracket => "[",
            TokenKind::RBracket => "]",
            TokenKind::LBrace => "{",
            TokenKind::RBrace => "}",
            TokenKind::Comma => ",",
            TokenKind::Semi => ";",
            TokenKind::Colon => ":",
            TokenKind::Dot => ".",
            TokenKind::Ellipsis => "...",
            TokenKind::Question => "?",
            TokenKind::SafeDot => "?.",
            TokenKind::At => "@",
            TokenKind::FatArrow => "=>",
            TokenKind::Arrow => "->",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::Amp => "&",
            TokenKind::Pipe => "|",
            TokenKind::Caret => "^",
            TokenKind::Tilde => "~",
            TokenKind::Bang => "!",
            TokenKind::Shl => "<<",
            TokenKind::Shr => ">>",
            TokenKind::UShr => ">>>",
            TokenKind::Lt => "<",
            TokenKind::Gt => ">",
            TokenKind::Le => "<=",
            TokenKind::Ge => ">=",
            TokenKind::EqEq => "==",
            TokenKind::NotEq => "!=",
            TokenKind::AmpAmp => "&&",
            TokenKind::PipePipe => "||",
            TokenKind::Coalesce => "??",
            TokenKind::PlusPlus => "++",
            TokenKind::MinusMinus => "--",
            TokenKind::Assign => "=",
            TokenKind::AssignOp(op) => match op {
                AssignOp::Add => "+=",
                AssignOp::Sub => "-=",
                AssignOp::Mul => "*=",
                AssignOp::Div => "/=",
                AssignOp::Rem => "%=",
                AssignOp::And => "&=",
                AssignOp::Or => "|=",
                AssignOp::Xor => "^=",
                AssignOp::Shl => "<<=",
                AssignOp::Shr => ">>=",
                AssignOp::UShr => ">>>=",
                AssignOp::Coalesce => "??=",
            },
            _ => "?",
        }
    }
}
