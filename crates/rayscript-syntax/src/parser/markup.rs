//! Markup literals. Between tags the parser reads raw bytes (text is not tokens); inside `{...}` it
//! goes back to tokens for the expression, and picks the raw bytes up again after the `}`.

use super::Parser;
use crate::ast::*;
use crate::span::Span;
use crate::token::{Kw, TokenKind};

enum ChildEnd {
    /// `</name>` or `</>` was read.
    Close { name: Option<(String, Span)> },
    /// `<else>` was read.
    Else,
    Eof,
}

/// JSX's rule for text: a line's edges are trimmed where it meets a newline, empty lines go, and
/// the rest joins with single spaces. Text with no newline in it is kept as written.
fn jsx_text(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let last = lines.len() - 1;
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let mut l = line.replace(['\t', '\r'], " ");
        if i != 0 {
            l = l.trim_start_matches(' ').to_string();
        }
        if i != last {
            l = l.trim_end_matches(' ').to_string();
        }
        if l.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&l);
    }
    if out.is_empty() { None } else { Some(out) }
}

impl<'a> Parser<'a> {
    /// At a `<` that begins an expression.
    pub(crate) fn parse_markup_expr(&mut self) -> Expr {
        let start = self.cur.span.start;
        self.lex.set_pos(self.cur.span.end as usize);
        self.lex.skip_trivia();
        let starts_tag = self.lex.peek_byte() == Some(b'>') || self.lex.scan_markup_name(true).map(|(_, s)| self.lex.set_pos(s.start as usize)).is_some();
        if !starts_tag {
            let span = self.cur.span;
            self.error_with(span, "expected a tag name after `<`", Some("a comparison needs a value on its left"));
            let end = self.cur.span.end as usize;
            self.resume_at(end);
            return Expr::new(ExprKind::Error, span);
        }
        let node = self.markup_after_lt(start);
        let end = self.lex.pos();
        self.resume_at(end);
        Expr::new(ExprKind::Markup(Box::new(node)), Span::new(start, end as u32))
    }

    /// Reads the expression of a `{...}` whose `{` was just consumed raw, and leaves the raw position after the `}`.
    fn markup_braced_expr(&mut self, open: usize) -> Expr {
        self.cur = self.lex.next_token();
        self.prev_end = (open + 1) as u32;
        self.prev_rbrace = false;
        let e = self.parse_expr();
        self.close_markup_brace();
        e
    }

    /// With the expression read, the current token should be the `}`; the raw position goes after it.
    fn close_markup_brace(&mut self) {
        if self.cur.kind == TokenKind::RBrace {
            self.lex.set_pos(self.cur.span.end as usize);
            return;
        }
        let found = self.cur.kind.describe();
        let span = self.cur.span;
        self.error(span, format!("expected `}}` to end the expression, found {found}"));
        // Go on from the next `}` if there is one, so the rest of the markup still reads.
        let rest = &self.lex.src()[span.start as usize..];
        let to = rest.find('}').map(|i| span.start as usize + i + 1).unwrap_or_else(|| self.lex.src().len());
        self.lex.set_pos(to);
    }

    fn raw_expect(&mut self, byte: u8, what: &str) -> bool {
        self.lex.skip_trivia();
        if self.lex.peek_byte() == Some(byte) {
            self.lex.bump_byte();
            return true;
        }
        let pos = self.lex.pos();
        let found = match self.lex.peek_byte() {
            None => "the end of the script".to_string(),
            Some(_) => self.lex.src()[pos..].chars().next().map(|c| format!("`{c}`")).unwrap_or_default(),
        };
        let span = Span::new(pos as u32, (pos + 1).min(self.lex.src().len()) as u32);
        self.error(span, format!("expected {what}, found {found}"));
        false
    }

    /// The lexer is just after the `<`. Reads an element, fragment or control tag to its end.
    fn markup_after_lt(&mut self, start: u32) -> MarkupNode {
        if self.depth >= super::MAX_DEPTH {
            let at = Span::new(start, start + 1);
            self.error(at, "this is nested too deeply to read");
            let end = self.lex.src().len();
            self.lex.set_pos(end);
            return MarkupNode::Fragment { children: Vec::new(), span: at };
        }
        self.depth_guard_enter();
        self.lex.skip_trivia();
        if self.lex.peek_byte() == Some(b'>') {
            self.lex.bump_byte();
            let (children, end) = self.markup_children();
            let open = Span::new(start, start + 2);
            self.finish_close(end, None, open);
            self.depth_guard_leave();
            return MarkupNode::Fragment { children, span: Span::new(start, self.lex.pos() as u32) };
        }
        let (name, name_span) = self.lex.scan_markup_name(true).expect("checked by the caller");
        let node = match name.as_str() {
            "if" => self.markup_if(start, name_span),
            "for" => self.markup_for(start, name_span),
            "else" => {
                self.error_with(name_span, "`<else>` is only for inside `<if>`", None);
                self.skip_to_tag_end();
                MarkupNode::Fragment { children: Vec::new(), span: Span::new(start, self.lex.pos() as u32) }
            }
            _ => self.markup_element(start, name, name_span),
        };
        self.depth_guard_leave();
        node
    }

    fn depth_guard_enter(&mut self) {
        self.depth += 1;
    }

    fn depth_guard_leave(&mut self) {
        self.depth -= 1;
    }

    fn skip_to_tag_end(&mut self) {
        while let Some(b) = self.lex.peek_byte() {
            self.lex.bump_byte();
            if b == b'>' {
                break;
            }
        }
    }

    fn markup_element(&mut self, start: u32, name: String, name_span: Span) -> MarkupNode {
        let tag = Ident { name: name.clone(), span: name_span };
        let mut attrs = Vec::new();
        let mut self_closing = false;
        loop {
            self.lex.skip_trivia();
            match self.lex.peek_byte() {
                None => {
                    self.error_with(Span::new(start, name_span.end), format!("this `<{name}` tag is never finished"), Some("end it with `>` or `/>`"));
                    return MarkupNode::Element(Element { tag, attrs, children: Vec::new(), self_closing: false, span: Span::new(start, self.lex.pos() as u32) });
                }
                Some(b'/') if self.lex.peek_byte_at(1) == Some(b'>') => {
                    self.lex.bump_byte();
                    self.lex.bump_byte();
                    self_closing = true;
                    break;
                }
                Some(b'>') => {
                    self.lex.bump_byte();
                    break;
                }
                Some(b'{') => {
                    let astart = self.lex.pos();
                    self.lex.bump_byte();
                    self.cur = self.lex.next_token();
                    if self.cur.kind == TokenKind::Ellipsis {
                        self.cur = self.lex.next_token();
                    } else {
                        let span = self.cur.span;
                        self.error_with(span, "expected `...` to spread an object into the attributes", Some("write {...props}"));
                    }
                    let expr = self.parse_expr();
                    self.close_markup_brace();
                    attrs.push(Attr::Spread { expr, span: Span::new(astart as u32, self.lex.pos() as u32) });
                }
                Some(_) => match self.lex.scan_markup_name(false) {
                    Some((aname, aspan)) => {
                        let ident = Ident { name: aname, span: aspan };
                        self.lex.skip_trivia();
                        let value = if self.lex.peek_byte() == Some(b'=') {
                            self.lex.bump_byte();
                            self.lex.skip_trivia();
                            self.markup_attr_value()
                        } else {
                            AttrValue::Flag
                        };
                        attrs.push(Attr::Named { name: ident, value, span: Span::new(aspan.start, self.lex.pos() as u32) });
                    }
                    None => {
                        let pos = self.lex.pos();
                        let c = self.lex.src()[pos..].chars().next().unwrap_or(' ');
                        self.error(Span::new(pos as u32, (pos + c.len_utf8()) as u32), format!("expected an attribute name, found `{c}`"));
                        self.lex.set_pos(pos + c.len_utf8());
                    }
                },
            }
        }
        let mut children = Vec::new();
        if !self_closing {
            let (kids, end) = self.markup_children();
            children = kids;
            let open = Span::new(start, name_span.end);
            self.finish_close(end, Some((&name, name_span)), open);
        }
        MarkupNode::Element(Element { tag, attrs, children, self_closing, span: Span::new(start, self.lex.pos() as u32) })
    }

    fn markup_attr_value(&mut self) -> AttrValue {
        match self.lex.peek_byte() {
            Some(b'"') | Some(b'\'') => {
                let t = self.lex.next_token();
                match t.kind {
                    TokenKind::Str(s) => AttrValue::Str(s),
                    TokenKind::Interp(parts) => AttrValue::Interp(self.interp_parts(parts)),
                    _ => AttrValue::Flag,
                }
            }
            Some(b'{') => {
                let open = self.lex.pos();
                self.lex.bump_byte();
                AttrValue::Expr(self.markup_braced_expr(open))
            }
            _ => {
                let pos = self.lex.pos();
                self.error_with(Span::new(pos as u32, pos as u32 + 1), "expected a value after `=`", Some("use a string like \"text\", or {expression}"));
                AttrValue::Flag
            }
        }
    }

    /// After the children: the closing tag has been read; checks its name against the opening tag's.
    fn finish_close(&mut self, end: ChildEnd, open: Option<(&str, Span)>, open_span: Span) {
        match end {
            ChildEnd::Close { name } => match (&name, open) {
                (Some((n, span)), Some((expected, _))) if n != expected => {
                    let opened = format!("`<{expected}>` is opened here");
                    self.error_labeled(*span, format!("this closes `</{n}>`, but the open tag is `<{expected}>`"), Some("the closing tag must have the same name"), &[(open_span, &opened)]);
                }
                (Some((n, span)), None) => {
                    self.error_labeled(*span, format!("this closes `</{n}>`, but the open tag is a fragment `<>`"), None, &[(open_span, "the fragment is opened here")]);
                }
                (None, Some((expected, _))) => {
                    self.error(open_span, format!("`<{expected}>` is closed with `</>`; close it with `</{expected}>`"));
                }
                _ => {}
            },
            ChildEnd::Else => {
                self.error(open_span, "`<else>` is only for inside `<if>`");
            }
            ChildEnd::Eof => {
                let what = open.map(|(n, _)| format!("<{n}>")).unwrap_or_else(|| "<>".to_string());
                self.error_with(open_span, format!("`{what}` is never closed"), Some("add the closing tag"));
            }
        }
    }

    /// Children up to the closing tag (which is consumed) or an `<else>`.
    fn markup_children(&mut self) -> (Vec<MarkupNode>, ChildEnd) {
        let mut children = Vec::new();
        loop {
            if let Some((text, span)) = self.lex.scan_markup_text() {
                if let Some(t) = jsx_text(&text) {
                    children.push(MarkupNode::Text { text: t, span });
                }
            }
            match self.lex.peek_byte() {
                None => return (children, ChildEnd::Eof),
                Some(b'{') => {
                    let open = self.lex.pos();
                    self.lex.bump_byte();
                    let expr = self.markup_braced_expr(open);
                    children.push(MarkupNode::Hole { expr, span: Span::new(open as u32, self.lex.pos() as u32) });
                }
                Some(b'<') => {
                    let lt = self.lex.pos();
                    if self.lex.peek_byte_at(1) == Some(b'/') {
                        self.lex.bump_byte();
                        self.lex.bump_byte();
                        self.lex.skip_trivia();
                        let name = self.lex.scan_markup_name(true);
                        self.raw_expect(b'>', "`>` to end the closing tag");
                        return (children, ChildEnd::Close { name });
                    }
                    self.lex.bump_byte();
                    // `<else>` ends the branch before it.
                    let after_lt = self.lex.pos();
                    self.lex.skip_trivia();
                    if let Some((n, _)) = self.lex.scan_markup_name(false) {
                        if n == "else" {
                            self.lex.skip_trivia();
                            if self.lex.peek_byte() == Some(b'>') {
                                self.lex.bump_byte();
                                return (children, ChildEnd::Else);
                            }
                        }
                    }
                    self.lex.set_pos(after_lt);
                    self.lex.skip_trivia();
                    let starts_tag = self.lex.peek_byte() == Some(b'>') || self.lex.scan_markup_name(true).map(|(_, s)| self.lex.set_pos(s.start as usize)).is_some();
                    if !starts_tag {
                        self.error_with(Span::new(lt as u32, lt as u32 + 1), "expected a tag name after `<`", Some("write &lt; for a literal <"));
                        continue;
                    }
                    let child = self.markup_after_lt(lt as u32);
                    children.push(child);
                }
                Some(_) => unreachable!("text stops only at `<`, `{{` or the end"),
            }
        }
    }

    fn markup_if(&mut self, start: u32, name_span: Span) -> MarkupNode {
        let cond = if self.raw_expect(b'{', "`{condition}` after `<if`") {
            let open = self.lex.pos() - 1;
            self.markup_braced_expr(open)
        } else {
            Expr::new(ExprKind::Error, name_span)
        };
        self.raw_expect(b'>', "`>` to end the `<if>` tag");
        let (then, end) = self.markup_children();
        let (els, end) = match end {
            ChildEnd::Else => {
                let (e, end2) = self.markup_children();
                (Some(e), end2)
            }
            other => (None, other),
        };
        let open = Span::new(start, name_span.end);
        self.finish_close(end, Some(("if", name_span)), open);
        MarkupNode::If { cond, then, els, span: Span::new(start, self.lex.pos() as u32) }
    }

    fn markup_for(&mut self, start: u32, name_span: Span) -> MarkupNode {
        let (key, var, iter) = if self.raw_expect(b'{', "`{x in list}` after `<for`") {
            self.cur = self.lex.next_token();
            let first = self.expect_ident("a loop variable");
            let (key, var) = if self.eat(&TokenKind::FatArrow) { (Some(first), self.expect_ident("a loop variable")) } else { (None, first) };
            self.expect_kw(Kw::In);
            let iter = self.parse_expr();
            self.close_markup_brace();
            (key, var, iter)
        } else {
            (None, Ident { name: String::new(), span: name_span }, Expr::new(ExprKind::Error, name_span))
        };
        self.raw_expect(b'>', "`>` to end the `<for>` tag");
        let (body, end) = self.markup_children();
        let open = Span::new(start, name_span.end);
        self.finish_close(end, Some(("for", name_span)), open);
        MarkupNode::For { key, var, iter, body, span: Span::new(start, self.lex.pos() as u32) }
    }
}
