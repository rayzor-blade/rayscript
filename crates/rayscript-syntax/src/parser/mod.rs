//! The parser: hand-written, recursive descent for statements and Pratt for expressions. It always
//! produces a tree, putting `Error` nodes where it could not read, and reports what it found.

mod markup;
mod pattern;
mod types;

use crate::ast::*;
use crate::lexer::Lexer;
use crate::span::{Diagnostic, Span};
use crate::token::{AssignOp, Kw, StrPart, Token, TokenKind};

pub(crate) const MAX_DEPTH: u32 = 96;

pub struct Parser<'a> {
    pub(crate) lex: Lexer<'a>,
    pub(crate) cur: Token,
    pub(crate) prev_end: u32,
    pub(crate) prev_rbrace: bool,
    pub(crate) diags: Vec<Diagnostic>,
    last_error: Option<u32>,
    depth: u32,
}

pub(crate) struct Checkpoint {
    pos: usize,
    cur: Token,
    prev_end: u32,
    prev_rbrace: bool,
    diags: usize,
    lex_diags: usize,
    last_error: Option<u32>,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Parser<'a> {
        Parser::over(Lexer::new(src))
    }

    fn over(mut lex: Lexer<'a>) -> Parser<'a> {
        let cur = lex.next_token();
        Parser { lex, cur, prev_end: 0, prev_rbrace: false, diags: Vec::new(), last_error: None, depth: 0 }
    }

    pub fn src(&self) -> &'a str {
        self.lex.src()
    }

    /// Everything reported, the lexer's and the parser's, in source order.
    pub fn finish(mut self) -> Vec<Diagnostic> {
        self.diags.append(&mut self.lex.diags);
        self.diags.sort_by_key(|d| (d.span.start, d.span.end));
        self.diags.dedup_by(|a, b| a.span == b.span && a.message == b.message);
        self.diags
    }

    // Tokens

    pub(crate) fn advance(&mut self) -> Token {
        let next = self.lex.next_token();
        let t = std::mem::replace(&mut self.cur, next);
        self.prev_end = t.span.end;
        self.prev_rbrace = t.kind == TokenKind::RBrace;
        t
    }

    /// The token after the current one, without moving.
    pub(crate) fn peek(&mut self) -> TokenKind {
        let pos = self.lex.pos();
        let diags = self.lex.diags.len();
        let t = self.lex.next_token();
        self.lex.set_pos(pos);
        self.lex.diags.truncate(diags);
        t.kind
    }

    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            pos: self.lex.pos(),
            cur: self.cur.clone(),
            prev_end: self.prev_end,
            prev_rbrace: self.prev_rbrace,
            diags: self.diags.len(),
            lex_diags: self.lex.diags.len(),
            last_error: self.last_error,
        }
    }

    pub(crate) fn restore(&mut self, c: Checkpoint) {
        self.lex.set_pos(c.pos);
        self.cur = c.cur;
        self.prev_end = c.prev_end;
        self.prev_rbrace = c.prev_rbrace;
        self.diags.truncate(c.diags);
        self.lex.diags.truncate(c.lex_diags);
        self.last_error = c.last_error;
    }

    /// Takes the lexer back to `pos` and reads the token there, for when markup has read raw bytes.
    pub(crate) fn resume_at(&mut self, pos: usize) {
        self.lex.set_pos(pos);
        self.prev_end = pos as u32;
        self.prev_rbrace = false;
        self.cur = self.lex.next_token();
    }

    pub(crate) fn is(&self, k: &TokenKind) -> bool {
        &self.cur.kind == k
    }

    pub(crate) fn is_kw(&self, k: Kw) -> bool {
        self.cur.kind == TokenKind::Kw(k)
    }

    pub(crate) fn eat(&mut self, k: &TokenKind) -> bool {
        if self.is(k) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(crate) fn eat_kw(&mut self, k: Kw) -> bool {
        if self.is_kw(k) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(crate) fn error(&mut self, span: Span, message: impl Into<String>) {
        self.error_with(span, message, None);
    }

    pub(crate) fn error_with(&mut self, span: Span, message: impl Into<String>, help: Option<&str>) {
        // One report for a place: what follows a bad token is usually noise.
        if self.last_error == Some(span.start) {
            return;
        }
        self.last_error = Some(span.start);
        let mut d = Diagnostic::error(message, span);
        if let Some(h) = help {
            d = d.with_help(h);
        }
        self.diags.push(d);
    }

    pub(crate) fn expect(&mut self, k: &TokenKind) -> bool {
        if self.eat(k) {
            return true;
        }
        let found = self.cur.kind.describe();
        let span = self.cur.span;
        self.error(span, format!("expected `{}`, found {}", k.symbol(), found));
        false
    }

    pub(crate) fn expect_kw(&mut self, k: Kw) -> bool {
        if self.eat_kw(k) {
            return true;
        }
        let found = self.cur.kind.describe();
        let span = self.cur.span;
        self.error(span, format!("expected `{}`, found {}", k.as_str(), found));
        false
    }

    pub(crate) fn expect_ident(&mut self, what: &str) -> Ident {
        match &self.cur.kind {
            TokenKind::Ident(name) => {
                let id = Ident { name: name.clone(), span: self.cur.span };
                self.advance();
                id
            }
            other => {
                let found = other.describe();
                let span = self.cur.span;
                self.error(span, format!("expected {what}, found {found}"));
                Ident { name: String::new(), span: Span::new(span.start, span.start) }
            }
        }
    }

    /// A name after `.`: an identifier, or a keyword used as a field name.
    fn field_name(&mut self) -> Ident {
        match &self.cur.kind {
            TokenKind::Kw(k) => {
                let id = Ident { name: k.as_str().to_string(), span: self.cur.span };
                self.advance();
                id
            }
            _ => self.expect_ident("a field name"),
        }
    }

    // Module and items

    pub fn parse_module(&mut self) -> Module {
        let start = self.cur.span.start;
        let mut items = Vec::new();
        while self.cur.kind != TokenKind::Eof {
            let before = self.cur.span.start;
            items.push(self.parse_item());
            if self.cur.span.start == before {
                // Nothing was consumed: step over the token so parsing always ends.
                self.advance();
            }
        }
        Module { items, span: Span::new(start, self.cur.span.end) }
    }

    fn parse_item(&mut self) -> Item {
        match self.cur.kind {
            TokenKind::Kw(Kw::Import) => Item::Import(self.parse_import()),
            TokenKind::Kw(Kw::Native) => Item::Native(self.parse_native()),
            _ => Item::Stmt(self.parse_stmt()),
        }
    }

    fn parse_import(&mut self) -> Import {
        let start = self.cur.span.start;
        self.advance();
        let mut path = vec![self.expect_ident("a module name")];
        let mut wildcard = false;
        while self.eat(&TokenKind::Dot) {
            if self.eat(&TokenKind::Star) {
                wildcard = true;
                break;
            }
            path.push(self.expect_ident("a name"));
        }
        let mut alias = None;
        if matches!(&self.cur.kind, TokenKind::Ident(n) if n == "as") {
            self.advance();
            alias = Some(self.expect_ident("a name for the import"));
        }
        self.expect_semi();
        Import { path, alias, wildcard, span: Span::new(start, self.prev_end) }
    }

    fn parse_native(&mut self) -> NativeBlock {
        let start = self.cur.span.start;
        self.advance();
        let (library, library_span) = match &self.cur.kind {
            TokenKind::Str(s) => {
                let r = (s.clone(), self.cur.span);
                self.advance();
                r
            }
            other => {
                let found = other.describe();
                let span = self.cur.span;
                self.error_with(span, format!("expected the name of a library in quotes, found {found}"), Some("write native \"libname\" { function f(a:Int):Int; }"));
                (String::new(), span)
            }
        };
        let mut functions = Vec::new();
        if self.expect(&TokenKind::LBrace) {
            while !self.is(&TokenKind::RBrace) && !self.is(&TokenKind::Eof) {
                let before = self.cur.span.start;
                let meta = self.parse_meta();
                if self.is_kw(Kw::Function) {
                    functions.push(self.parse_native_fn(meta));
                } else {
                    let found = self.cur.kind.describe();
                    let span = self.cur.span;
                    self.error(span, format!("expected `function` in a native block, found {found}"));
                    self.synchronize();
                }
                if self.cur.span.start == before {
                    self.advance();
                }
            }
            self.expect(&TokenKind::RBrace);
        }
        NativeBlock { library, library_span, functions, span: Span::new(start, self.prev_end) }
    }

    fn parse_native_fn(&mut self, meta: Vec<Meta>) -> NativeFn {
        let start = self.cur.span.start;
        self.advance();
        let name = self.expect_ident("a function name");
        let params = self.parse_params();
        let ret = if self.eat(&TokenKind::Colon) { Some(self.parse_type()) } else { None };
        self.expect(&TokenKind::Semi);
        NativeFn { meta, name, params, ret, span: Span::new(start, self.prev_end) }
    }

    // Statements

    /// The `;` that ends a statement. It may be left out before `}` and the end, and after a `}`.
    fn expect_semi(&mut self) {
        if self.eat(&TokenKind::Semi) || self.is(&TokenKind::RBrace) || self.is(&TokenKind::Eof) || self.prev_rbrace {
            return;
        }
        let at = Span::new(self.prev_end, self.prev_end);
        let found = self.cur.kind.describe();
        self.error_with(at, format!("expected `;` before {found}"), Some("end the statement with a semicolon"));
    }

    pub(crate) fn parse_stmt(&mut self) -> Expr {
        let before = self.cur.span.start;
        let e = self.parse_expr();
        if matches!(e.kind, ExprKind::Error) {
            self.synchronize();
        } else {
            self.expect_semi();
        }
        if self.cur.span.start == before {
            self.advance();
        }
        e
    }

    /// After an error, skips to the end of the statement: past a `;`, or up to a `}`.
    fn synchronize(&mut self) {
        let mut braces = 0i32;
        loop {
            match &self.cur.kind {
                TokenKind::Eof => return,
                TokenKind::Semi if braces <= 0 => {
                    self.advance();
                    return;
                }
                TokenKind::RBrace => {
                    if braces <= 0 {
                        return;
                    }
                    braces -= 1;
                }
                TokenKind::LBrace => braces += 1,
                TokenKind::Kw(Kw::Var | Kw::Final | Kw::Function | Kw::Import | Kw::Native) if braces <= 0 && self.cur.newline_before => return,
                _ => {}
            }
            self.advance();
        }
    }

    pub(crate) fn parse_block(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.expect(&TokenKind::LBrace);
        let mut stmts = Vec::new();
        while !self.is(&TokenKind::RBrace) && !self.is(&TokenKind::Eof) {
            stmts.push(self.parse_stmt());
        }
        if !self.eat(&TokenKind::RBrace) {
            let span = Span::new(start, start + 1);
            self.error_with(span, "this `{` is never closed", Some("add the matching `}`"));
        }
        Expr::new(ExprKind::Block(stmts), Span::new(start, self.prev_end))
    }

    /// The body of a function, loop or branch: a block unless the braces read as an object.
    pub(crate) fn parse_body(&mut self) -> Expr {
        if self.is(&TokenKind::LBrace) && !self.brace_is_object() {
            return self.parse_block();
        }
        self.parse_expr()
    }

    /// Whether the `{` here begins an object: `{ name: ...`, `{ "str": ...`.
    fn brace_is_object(&mut self) -> bool {
        let c = self.checkpoint();
        self.advance();
        let is_object = match &self.cur.kind {
            TokenKind::Ident(_) | TokenKind::Str(_) | TokenKind::Kw(_) => self.peek() == TokenKind::Colon,
            _ => false,
        };
        self.restore(c);
        is_object
    }

    // Expressions

    pub fn parse_expr(&mut self) -> Expr {
        self.parse_bp(0)
    }

    fn parse_bp(&mut self, min_bp: u8) -> Expr {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            let span = self.cur.span;
            self.error(span, "this is nested too deeply to read");
            self.depth -= 1;
            return Expr::new(ExprKind::Error, span);
        }
        let mut left = self.parse_prefix();
        loop {
            let start = left.span.start;
            match self.cur.kind.clone() {
                TokenKind::Dot | TokenKind::SafeDot => {
                    let safe = self.cur.kind == TokenKind::SafeDot;
                    self.advance();
                    let name = self.field_name();
                    left = Expr::new(ExprKind::Field { obj: Box::new(left), name, safe }, Span::new(start, self.prev_end));
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.parse_expr();
                    self.expect(&TokenKind::RBracket);
                    left = Expr::new(ExprKind::Index { obj: Box::new(left), index: Box::new(index) }, Span::new(start, self.prev_end));
                }
                TokenKind::LParen => {
                    let args = self.parse_args();
                    left = Expr::new(ExprKind::Call { callee: Box::new(left), args }, Span::new(start, self.prev_end));
                }
                TokenKind::PlusPlus | TokenKind::MinusMinus if !self.cur.newline_before => {
                    let op = if self.cur.kind == TokenKind::PlusPlus { UnOp::PostInc } else { UnOp::PostDec };
                    self.advance();
                    self.check_assignable(&left, "increment");
                    left = Expr::new(ExprKind::Unary { op, expr: Box::new(left) }, Span::new(start, self.prev_end));
                }
                TokenKind::Assign | TokenKind::AssignOp(_) => {
                    if 2 < min_bp {
                        break;
                    }
                    let op = match self.cur.kind {
                        TokenKind::AssignOp(a) => Some(assign_binop(a)),
                        _ => None,
                    };
                    self.advance();
                    self.check_assignable(&left, "assign to");
                    let value = self.parse_bp(2);
                    left = Expr::new(ExprKind::Assign { op, target: Box::new(left), value: Box::new(value) }, Span::new(start, self.prev_end));
                }
                TokenKind::Question => {
                    if 4 < min_bp {
                        break;
                    }
                    self.advance();
                    let then = self.parse_bp(0);
                    self.expect(&TokenKind::Colon);
                    let els = self.parse_bp(4);
                    left = Expr::new(ExprKind::Ternary { cond: Box::new(left), then: Box::new(then), els: Box::new(els) }, Span::new(start, self.prev_end));
                }
                kind => {
                    let Some((op, lbp, rbp)) = infix(&kind) else { break };
                    if lbp < min_bp {
                        break;
                    }
                    self.advance();
                    let right = self.parse_bp(rbp);
                    left = Expr::new(ExprKind::Binary { op, left: Box::new(left), right: Box::new(right) }, Span::new(start, self.prev_end));
                }
            }
        }
        self.depth -= 1;
        left
    }

    fn check_assignable(&mut self, target: &Expr, verb: &str) {
        let ok = match &target.kind {
            ExprKind::Ident(_) | ExprKind::Field { .. } | ExprKind::Index { .. } | ExprKind::Error => true,
            ExprKind::Paren(inner) => matches!(inner.kind, ExprKind::Ident(_) | ExprKind::Field { .. } | ExprKind::Index { .. }),
            _ => false,
        };
        if !ok {
            self.error(target.span, format!("cannot {verb} this"));
        }
    }

    pub(crate) fn parse_args(&mut self) -> Vec<Expr> {
        let mut args = Vec::new();
        self.expect(&TokenKind::LParen);
        while !self.is(&TokenKind::RParen) && !self.is(&TokenKind::Eof) {
            let before = self.cur.span.start;
            args.push(self.parse_expr());
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            if self.cur.span.start == before {
                self.advance();
            }
        }
        if !self.eat(&TokenKind::RParen) {
            let found = self.cur.kind.describe();
            let span = self.cur.span;
            self.error(span, format!("expected `)` or `,` in the arguments, found {found}"));
        }
        args
    }

    fn parse_prefix(&mut self) -> Expr {
        let start = self.cur.span.start;
        let span = self.cur.span;
        match self.cur.kind.clone() {
            TokenKind::Int(v) => {
                self.advance();
                Expr::new(ExprKind::Int(v), span)
            }
            TokenKind::Float(v) => {
                self.advance();
                Expr::new(ExprKind::Float(v), span)
            }
            TokenKind::Str(s) => {
                self.advance();
                Expr::new(ExprKind::Str(s), span)
            }
            TokenKind::Interp(parts) => {
                self.advance();
                let parts = self.interp_parts(parts);
                Expr::new(ExprKind::Interp(parts), span)
            }
            TokenKind::Kw(Kw::True) => {
                self.advance();
                Expr::new(ExprKind::Bool(true), span)
            }
            TokenKind::Kw(Kw::False) => {
                self.advance();
                Expr::new(ExprKind::Bool(false), span)
            }
            TokenKind::Kw(Kw::Null) => {
                self.advance();
                Expr::new(ExprKind::Null, span)
            }
            TokenKind::Ident(name) => {
                if self.peek() == TokenKind::Arrow {
                    return self.parse_single_param_lambda();
                }
                self.advance();
                Expr::new(ExprKind::Ident(name), span)
            }
            TokenKind::LParen => self.parse_paren_or_lambda(),
            TokenKind::LBracket => self.parse_array_or_map(),
            TokenKind::LBrace => {
                if self.brace_is_object() || self.brace_is_empty() {
                    self.parse_object()
                } else {
                    self.parse_block()
                }
            }
            TokenKind::Minus | TokenKind::Bang | TokenKind::Tilde | TokenKind::PlusPlus | TokenKind::MinusMinus => {
                let op = match self.cur.kind {
                    TokenKind::Minus => UnOp::Neg,
                    TokenKind::Bang => UnOp::Not,
                    TokenKind::Tilde => UnOp::BitNot,
                    TokenKind::PlusPlus => UnOp::PreInc,
                    _ => UnOp::PreDec,
                };
                self.advance();
                let operand = self.parse_bp(24);
                if matches!(op, UnOp::PreInc | UnOp::PreDec) {
                    self.check_assignable(&operand, "increment");
                }
                // `-1` stays a literal, so patterns and constants read it as one.
                if op == UnOp::Neg {
                    match operand.kind {
                        ExprKind::Int(v) => return Expr::new(ExprKind::Int(v.wrapping_neg()), Span::new(start, operand.span.end)),
                        ExprKind::Float(v) => return Expr::new(ExprKind::Float(-v), Span::new(start, operand.span.end)),
                        _ => {}
                    }
                }
                Expr::new(ExprKind::Unary { op, expr: Box::new(operand) }, Span::new(start, self.prev_end))
            }
            TokenKind::Kw(Kw::New) => self.parse_new(),
            TokenKind::Kw(Kw::Function) => self.parse_function(Vec::new(), Vec::new()),
            TokenKind::Kw(Kw::Var | Kw::Final) => self.parse_var(Vec::new(), Vec::new()),
            TokenKind::Kw(Kw::Public | Kw::Private | Kw::Static | Kw::Inline) => self.parse_modified(Vec::new()),
            TokenKind::At => {
                let meta = self.parse_meta();
                self.parse_with_meta(meta)
            }
            TokenKind::Kw(Kw::If) => self.parse_if(),
            TokenKind::Kw(Kw::While) => self.parse_while(),
            TokenKind::Kw(Kw::Do) => self.parse_do_while(),
            TokenKind::Kw(Kw::For) => self.parse_for(),
            TokenKind::Kw(Kw::Switch) => self.parse_switch(),
            TokenKind::Kw(Kw::Try) => self.parse_try(),
            TokenKind::Kw(Kw::Return) => {
                self.advance();
                let value = if self.expression_ends() { None } else { Some(Box::new(self.parse_expr())) };
                Expr::new(ExprKind::Return(value), Span::new(start, self.prev_end))
            }
            TokenKind::Kw(Kw::Break) => {
                self.advance();
                Expr::new(ExprKind::Break, span)
            }
            TokenKind::Kw(Kw::Continue) => {
                self.advance();
                Expr::new(ExprKind::Continue, span)
            }
            TokenKind::Kw(Kw::Throw) => {
                self.advance();
                let value = self.parse_expr();
                Expr::new(ExprKind::Throw(Box::new(value)), Span::new(start, self.prev_end))
            }
            TokenKind::Kw(Kw::Await) => {
                self.advance();
                let value = self.parse_bp(24);
                Expr::new(ExprKind::Await(Box::new(value)), Span::new(start, self.prev_end))
            }
            TokenKind::Lt => self.parse_markup_expr(),
            other => {
                let found = other.describe();
                self.error(span, format!("expected an expression, found {found}"));
                // Leave closing tokens to whoever is waiting for them.
                if !matches!(other, TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket | TokenKind::Semi | TokenKind::Eof) {
                    self.advance();
                }
                Expr::new(ExprKind::Error, span)
            }
        }
    }

    /// Whether what follows cannot start an expression: for `return` with no value.
    fn expression_ends(&self) -> bool {
        matches!(self.cur.kind, TokenKind::Semi | TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket | TokenKind::Comma | TokenKind::Eof | TokenKind::Kw(Kw::Else | Kw::Case | Kw::Default))
    }

    fn brace_is_empty(&mut self) -> bool {
        let c = self.checkpoint();
        self.advance();
        let empty = self.is(&TokenKind::RBrace);
        self.restore(c);
        empty
    }

    fn interp_parts(&mut self, parts: Vec<StrPart>) -> Vec<InterpPart> {
        let mut out = Vec::new();
        for part in parts {
            match part {
                StrPart::Lit(s) => out.push(InterpPart::Lit(s)),
                StrPart::Ident(name, span) => out.push(InterpPart::Expr(Expr::new(ExprKind::Ident(name), span))),
                StrPart::Expr(span) => out.push(InterpPart::Expr(self.sub_parse(span))),
            }
        }
        out
    }

    /// Parses the text of `span` as one expression: the inside of `${...}`.
    fn sub_parse(&mut self, span: Span) -> Expr {
        let lex = Lexer::range(self.lex.src(), span.start as usize, span.end as usize);
        let mut sub = Parser::over(lex);
        let e = sub.parse_expr();
        if sub.cur.kind != TokenKind::Eof {
            let found = sub.cur.kind.describe();
            let at = sub.cur.span;
            sub.error(at, format!("expected the end of the expression, found {found}"));
        }
        let mut diags = sub.finish();
        self.diags.append(&mut diags);
        e
    }

    // Literals

    fn parse_array_or_map(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.advance();
        if self.is_kw(Kw::For) || self.is_kw(Kw::While) {
            let (body, map) = self.parse_comp_body();
            self.expect(&TokenKind::RBracket);
            return Expr::new(ExprKind::Comprehension { map, body: Box::new(body) }, Span::new(start, self.prev_end));
        }
        let mut items: Vec<Expr> = Vec::new();
        let mut pairs: Vec<(Expr, Expr)> = Vec::new();
        let mut is_map = false;
        while !self.is(&TokenKind::RBracket) && !self.is(&TokenKind::Eof) {
            let before = self.cur.span.start;
            let first = self.parse_expr();
            if self.eat(&TokenKind::FatArrow) {
                is_map = true;
                let value = self.parse_expr();
                pairs.push((first, value));
            } else {
                if is_map {
                    self.error(first.span, "this entry has no `=>`; a map holds only `key => value` entries");
                }
                items.push(first);
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            if self.cur.span.start == before {
                self.advance();
            }
        }
        if !self.eat(&TokenKind::RBracket) {
            let found = self.cur.kind.describe();
            let span = self.cur.span;
            self.error(span, format!("expected `]` or `,`, found {found}"));
        }
        let span = Span::new(start, self.prev_end);
        if is_map {
            if !items.is_empty() {
                self.error(span, "a map cannot also hold plain values");
            }
            Expr::new(ExprKind::Map(pairs), span)
        } else {
            Expr::new(ExprKind::Array(items), span)
        }
    }

    /// The body of a comprehension: `for`, `while` and `if` around a value, or a `key => value` pair.
    fn parse_comp_body(&mut self) -> (Expr, bool) {
        let start = self.cur.span.start;
        if self.is_kw(Kw::For) {
            let (key, var, iter) = self.parse_for_header();
            let (body, map) = self.parse_comp_body();
            return (Expr::new(ExprKind::For { key, var, iter: Box::new(iter), body: Box::new(body) }, Span::new(start, self.prev_end)), map);
        }
        if self.is_kw(Kw::While) {
            self.advance();
            let cond = self.parse_paren_cond();
            let (body, map) = self.parse_comp_body();
            return (Expr::new(ExprKind::While { cond: Box::new(cond), body: Box::new(body) }, Span::new(start, self.prev_end)), map);
        }
        if self.is_kw(Kw::If) {
            self.advance();
            let cond = self.parse_paren_cond();
            let (then, map) = self.parse_comp_body();
            let mut map = map;
            let els = if self.eat_kw(Kw::Else) {
                let (e, m) = self.parse_comp_body();
                map |= m;
                Some(Box::new(e))
            } else {
                None
            };
            return (Expr::new(ExprKind::If { cond: Box::new(cond), then: Box::new(then), els }, Span::new(start, self.prev_end)), map);
        }
        let key = self.parse_expr();
        if self.eat(&TokenKind::FatArrow) {
            let value = self.parse_expr();
            let span = Span::new(start, self.prev_end);
            return (Expr::new(ExprKind::Map(vec![(key, value)]), span), true);
        }
        (key, false)
    }

    fn parse_object(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.advance();
        let mut fields = Vec::new();
        while !self.is(&TokenKind::RBrace) && !self.is(&TokenKind::Eof) {
            let before = self.cur.span.start;
            let (name, name_span) = match &self.cur.kind {
                TokenKind::Ident(n) => (n.clone(), self.cur.span),
                TokenKind::Str(s) => (s.clone(), self.cur.span),
                TokenKind::Kw(k) => (k.as_str().to_string(), self.cur.span),
                other => {
                    let found = other.describe();
                    let span = self.cur.span;
                    self.error(span, format!("expected a field name, found {found}"));
                    break;
                }
            };
            self.advance();
            self.expect(&TokenKind::Colon);
            let value = self.parse_expr();
            fields.push(ObjField { name, name_span, value });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            if self.cur.span.start == before {
                self.advance();
            }
        }
        if !self.eat(&TokenKind::RBrace) {
            let found = self.cur.kind.describe();
            let span = self.cur.span;
            self.error(span, format!("expected `}}` or `,` in the object, found {found}"));
        }
        Expr::new(ExprKind::Object(fields), Span::new(start, self.prev_end))
    }

    fn parse_new(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.advance();
        let mut path = vec![self.expect_ident("a type name")];
        while self.eat(&TokenKind::Dot) {
            path.push(self.expect_ident("a name"));
        }
        let args = if self.is(&TokenKind::LParen) { self.parse_args() } else { Vec::new() };
        Expr::new(ExprKind::New { path, args }, Span::new(start, self.prev_end))
    }

    // Declarations

    fn parse_meta(&mut self) -> Vec<Meta> {
        let mut metas = Vec::new();
        while self.is(&TokenKind::At) {
            let start = self.cur.span.start;
            self.advance();
            let mut name = String::new();
            if self.eat(&TokenKind::Colon) {
                name.push(':');
            }
            match &self.cur.kind {
                TokenKind::Ident(n) => {
                    name.push_str(n);
                    self.advance();
                }
                TokenKind::Kw(k) => {
                    name.push_str(k.as_str());
                    self.advance();
                }
                other => {
                    let found = other.describe();
                    let span = self.cur.span;
                    self.error(span, format!("expected the name of the metadata, found {found}"));
                }
            }
            let args = if self.is(&TokenKind::LParen) { self.parse_args() } else { Vec::new() };
            metas.push(Meta { name, args, span: Span::new(start, self.prev_end) });
        }
        metas
    }

    fn parse_with_meta(&mut self, meta: Vec<Meta>) -> Expr {
        match self.cur.kind {
            TokenKind::Kw(Kw::Function) => self.parse_function(Vec::new(), meta),
            TokenKind::Kw(Kw::Var | Kw::Final) => self.parse_var(Vec::new(), meta),
            TokenKind::Kw(Kw::Public | Kw::Private | Kw::Static | Kw::Inline) => self.parse_modified(meta),
            _ => self.parse_prefix(),
        }
    }

    fn parse_modified(&mut self, meta: Vec<Meta>) -> Expr {
        let mut modifiers = Vec::new();
        loop {
            let m = match self.cur.kind {
                TokenKind::Kw(Kw::Public) => Modifier::Public,
                TokenKind::Kw(Kw::Private) => Modifier::Private,
                TokenKind::Kw(Kw::Static) => Modifier::Static,
                TokenKind::Kw(Kw::Inline) => Modifier::Inline,
                _ => break,
            };
            modifiers.push(m);
            self.advance();
        }
        match self.cur.kind {
            TokenKind::Kw(Kw::Function) => self.parse_function(modifiers, meta),
            TokenKind::Kw(Kw::Var | Kw::Final) => self.parse_var(modifiers, meta),
            _ => {
                let found = self.cur.kind.describe();
                let span = self.cur.span;
                self.error(span, format!("expected `function` or `var` after the modifier, found {found}"));
                Expr::new(ExprKind::Error, span)
            }
        }
    }

    fn parse_var(&mut self, modifiers: Vec<Modifier>, meta: Vec<Meta>) -> Expr {
        let start = self.cur.span.start;
        let is_final = self.is_kw(Kw::Final);
        self.advance();
        let mut decls = Vec::new();
        loop {
            let dstart = self.cur.span.start;
            let name = self.expect_ident("a variable name");
            let ty = if self.eat(&TokenKind::Colon) { Some(self.parse_type()) } else { None };
            let init = if self.eat(&TokenKind::Assign) { Some(self.parse_expr()) } else { None };
            decls.push(VarDecl { name, ty, init, span: Span::new(dstart, self.prev_end) });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Expr::new(ExprKind::Var(Box::new(VarDecls { is_final, modifiers, meta, decls })), Span::new(start, self.prev_end))
    }

    pub(crate) fn parse_params(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        self.expect(&TokenKind::LParen);
        while !self.is(&TokenKind::RParen) && !self.is(&TokenKind::Eof) {
            let before = self.cur.span.start;
            params.push(self.parse_param());
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            if self.cur.span.start == before {
                self.advance();
            }
        }
        if !self.eat(&TokenKind::RParen) {
            let found = self.cur.kind.describe();
            let span = self.cur.span;
            self.error(span, format!("expected `)` or `,` in the parameters, found {found}"));
        }
        params
    }

    fn parse_param(&mut self) -> Param {
        let start = self.cur.span.start;
        let optional = self.eat(&TokenKind::Question);
        let rest = self.eat(&TokenKind::Ellipsis);
        let name = self.expect_ident("a parameter name");
        let ty = if self.eat(&TokenKind::Colon) { Some(self.parse_type()) } else { None };
        let default = if self.eat(&TokenKind::Assign) { Some(self.parse_expr()) } else { None };
        Param { name, ty, default, optional, rest, span: Span::new(start, self.prev_end) }
    }

    fn parse_function(&mut self, modifiers: Vec<Modifier>, meta: Vec<Meta>) -> Expr {
        let start = self.cur.span.start;
        self.advance();
        let name = if matches!(self.cur.kind, TokenKind::Ident(_)) { Some(self.expect_ident("a function name")) } else { None };
        let params = self.parse_params();
        let ret = if self.eat(&TokenKind::Colon) { Some(self.parse_type()) } else { None };
        let body = self.parse_body();
        let span = Span::new(start, self.prev_end);
        Expr::new(ExprKind::Function(Box::new(FnDecl { name, params, ret, body, modifiers, meta, span })), span)
    }

    fn parse_single_param_lambda(&mut self) -> Expr {
        let start = self.cur.span.start;
        let name = self.expect_ident("a parameter name");
        let pspan = name.span;
        self.expect(&TokenKind::Arrow);
        let body = self.parse_body();
        let span = Span::new(start, self.prev_end);
        let params = vec![Param { name, ty: None, default: None, optional: false, rest: false, span: pspan }];
        Expr::new(ExprKind::Lambda(Box::new(Lambda { params, ret: None, body, span })), span)
    }

    /// At `(`: a lambda's parameters if `) ->` follows them, otherwise a parenthesised expression.
    fn parse_paren_or_lambda(&mut self) -> Expr {
        let start = self.cur.span.start;
        let c = self.checkpoint();
        let params = self.parse_params();
        let clean = self.diags.len() == c.diags;
        let ret = if clean && self.is(&TokenKind::Colon) { self.advance(); Some(self.parse_type()) } else { None };
        if clean && self.diags.len() == c.diags && self.is(&TokenKind::Arrow) {
            self.advance();
            let body = self.parse_body();
            let span = Span::new(start, self.prev_end);
            return Expr::new(ExprKind::Lambda(Box::new(Lambda { params, ret, body, span })), span);
        }
        self.restore(c);
        self.advance();
        let inner = self.parse_expr();
        if !self.eat(&TokenKind::RParen) {
            let found = self.cur.kind.describe();
            let span = self.cur.span;
            self.error_with(span, format!("expected `)`, found {found}"), Some("this `(` is never closed"));
        }
        Expr::new(ExprKind::Paren(Box::new(inner)), Span::new(start, self.prev_end))
    }

    // Control flow

    pub(crate) fn parse_paren_cond(&mut self) -> Expr {
        self.expect(&TokenKind::LParen);
        let cond = self.parse_expr();
        self.expect(&TokenKind::RParen);
        cond
    }

    fn parse_if(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.advance();
        let cond = self.parse_paren_cond();
        let then = self.parse_body();
        // `if (a) b; else c`
        if self.is(&TokenKind::Semi) && self.peek() == TokenKind::Kw(Kw::Else) {
            self.advance();
        }
        let els = if self.eat_kw(Kw::Else) { Some(Box::new(self.parse_body())) } else { None };
        Expr::new(ExprKind::If { cond: Box::new(cond), then: Box::new(then), els }, Span::new(start, self.prev_end))
    }

    fn parse_while(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.advance();
        let cond = self.parse_paren_cond();
        let body = self.parse_body();
        Expr::new(ExprKind::While { cond: Box::new(cond), body: Box::new(body) }, Span::new(start, self.prev_end))
    }

    fn parse_do_while(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.advance();
        let body = self.parse_body();
        if self.is(&TokenKind::Semi) && self.peek() == TokenKind::Kw(Kw::While) {
            self.advance();
        }
        self.expect_kw(Kw::While);
        let cond = self.parse_paren_cond();
        Expr::new(ExprKind::DoWhile { body: Box::new(body), cond: Box::new(cond) }, Span::new(start, self.prev_end))
    }

    /// `for (x in xs)`, `for (k => v in map)`: the header up to and including `)`.
    pub(crate) fn parse_for_header(&mut self) -> (Option<Ident>, Ident, Expr) {
        self.advance();
        self.expect(&TokenKind::LParen);
        let first = self.expect_ident("a loop variable");
        let (key, var) = if self.eat(&TokenKind::FatArrow) { (Some(first), self.expect_ident("a loop variable")) } else { (None, first) };
        self.expect_kw(Kw::In);
        let iter = self.parse_expr();
        self.expect(&TokenKind::RParen);
        (key, var, iter)
    }

    fn parse_for(&mut self) -> Expr {
        let start = self.cur.span.start;
        let (key, var, iter) = self.parse_for_header();
        let body = self.parse_body();
        Expr::new(ExprKind::For { key, var, iter: Box::new(iter), body: Box::new(body) }, Span::new(start, self.prev_end))
    }

    fn parse_try(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.advance();
        let body = self.parse_body();
        if self.is(&TokenKind::Semi) && self.peek() == TokenKind::Kw(Kw::Catch) {
            self.advance();
        }
        let mut catches = Vec::new();
        while self.is_kw(Kw::Catch) {
            let cstart = self.cur.span.start;
            self.advance();
            self.expect(&TokenKind::LParen);
            let name = self.expect_ident("a name for the error");
            let ty = if self.eat(&TokenKind::Colon) { Some(self.parse_type()) } else { None };
            self.expect(&TokenKind::RParen);
            let cbody = self.parse_body();
            catches.push(Catch { name, ty, body: cbody, span: Span::new(cstart, self.prev_end) });
        }
        if catches.is_empty() {
            let span = Span::new(start, start + 3);
            self.error_with(span, "this `try` has no `catch`", Some("add catch (e) { ... }"));
        }
        Expr::new(ExprKind::Try { body: Box::new(body), catches }, Span::new(start, self.prev_end))
    }

    fn parse_switch(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.advance();
        let subject = self.parse_paren_cond();
        let mut cases = Vec::new();
        let mut default = None;
        if self.expect(&TokenKind::LBrace) {
            while !self.is(&TokenKind::RBrace) && !self.is(&TokenKind::Eof) {
                let before = self.cur.span.start;
                if self.is_kw(Kw::Case) {
                    cases.push(self.parse_case());
                } else if self.is_kw(Kw::Default) {
                    self.advance();
                    self.expect(&TokenKind::Colon);
                    default = Some(Box::new(self.parse_case_body()));
                } else {
                    let found = self.cur.kind.describe();
                    let span = self.cur.span;
                    self.error(span, format!("expected `case` or `default`, found {found}"));
                    self.synchronize();
                }
                if self.cur.span.start == before {
                    self.advance();
                }
            }
            self.expect(&TokenKind::RBrace);
        }
        Expr::new(ExprKind::Switch { subject: Box::new(subject), cases, default }, Span::new(start, self.prev_end))
    }

    fn parse_case(&mut self) -> Case {
        let start = self.cur.span.start;
        self.advance();
        let patterns = self.parse_case_patterns();
        let guard = if self.eat_kw(Kw::If) { Some(self.parse_expr()) } else { None };
        self.expect(&TokenKind::Colon);
        let body = self.parse_case_body();
        Case { patterns, guard, body, span: Span::new(start, self.prev_end) }
    }

    /// The statements of a case, up to the next `case`, `default` or `}`: one expression stays itself.
    fn parse_case_body(&mut self) -> Expr {
        let start = self.cur.span.start;
        let mut stmts = Vec::new();
        while !self.is_kw(Kw::Case) && !self.is_kw(Kw::Default) && !self.is(&TokenKind::RBrace) && !self.is(&TokenKind::Eof) {
            stmts.push(self.parse_stmt());
        }
        let span = Span::new(start, self.prev_end.max(start));
        match stmts.len() {
            1 => stmts.pop().unwrap(),
            _ => Expr::new(ExprKind::Block(stmts), span),
        }
    }
}

fn assign_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Rem => BinOp::Rem,
        AssignOp::And => BinOp::BitAnd,
        AssignOp::Or => BinOp::BitOr,
        AssignOp::Xor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
        AssignOp::UShr => BinOp::UShr,
        AssignOp::Coalesce => BinOp::Coalesce,
    }
}

/// The binary operator a token is, with its binding powers (left, right): higher binds tighter.
fn infix(kind: &TokenKind) -> Option<(BinOp, u8, u8)> {
    Some(match kind {
        TokenKind::Coalesce => (BinOp::Coalesce, 6, 6),
        TokenKind::PipePipe => (BinOp::Or, 8, 9),
        TokenKind::AmpAmp => (BinOp::And, 10, 11),
        TokenKind::Ellipsis => (BinOp::Interval, 12, 13),
        TokenKind::EqEq => (BinOp::Eq, 14, 15),
        TokenKind::NotEq => (BinOp::NotEq, 14, 15),
        TokenKind::Lt => (BinOp::Lt, 14, 15),
        TokenKind::Le => (BinOp::Le, 14, 15),
        TokenKind::Gt => (BinOp::Gt, 14, 15),
        TokenKind::Ge => (BinOp::Ge, 14, 15),
        TokenKind::Pipe => (BinOp::BitOr, 16, 17),
        TokenKind::Amp => (BinOp::BitAnd, 16, 17),
        TokenKind::Caret => (BinOp::BitXor, 16, 17),
        TokenKind::Shl => (BinOp::Shl, 18, 19),
        TokenKind::Shr => (BinOp::Shr, 18, 19),
        TokenKind::UShr => (BinOp::UShr, 18, 19),
        TokenKind::Plus => (BinOp::Add, 20, 21),
        TokenKind::Minus => (BinOp::Sub, 20, 21),
        TokenKind::Star => (BinOp::Mul, 22, 23),
        TokenKind::Slash => (BinOp::Div, 22, 23),
        TokenKind::Percent => (BinOp::Rem, 22, 23),
        _ => return None,
    })
}
