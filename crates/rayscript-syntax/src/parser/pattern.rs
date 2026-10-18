//! Patterns of `switch` cases.

use super::Parser;
use crate::ast::*;
use crate::span::Span;
use crate::token::TokenKind;

impl<'a> Parser<'a> {
    /// The patterns of a case, separated by `,` or `|`: any of them makes the case.
    pub(crate) fn parse_case_patterns(&mut self) -> Vec<Pattern> {
        let mut patterns = vec![self.parse_pattern()];
        while self.eat(&TokenKind::Comma) || self.eat(&TokenKind::Pipe) {
            patterns.push(self.parse_pattern());
        }
        patterns
    }

    fn parse_pattern(&mut self) -> Pattern {
        let start = self.cur.span.start;
        let span = self.cur.span;
        match self.cur.kind.clone() {
            TokenKind::Ident(name) => {
                if name == "_" {
                    self.advance();
                    return Pattern { kind: PatternKind::Wildcard, span };
                }
                let capitalised = name.chars().next().is_some_and(|c| c.is_uppercase());
                let follows_path = matches!(self.peek(), TokenKind::LParen | TokenKind::Dot);
                if capitalised || follows_path {
                    let mut path = vec![self.expect_ident("a name")];
                    while self.eat(&TokenKind::Dot) {
                        path.push(self.expect_ident("a name"));
                    }
                    let mut args = Vec::new();
                    if self.eat(&TokenKind::LParen) {
                        while !self.is(&TokenKind::RParen) && !self.is(&TokenKind::Eof) {
                            args.push(self.parse_pattern());
                            if !self.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        self.expect(&TokenKind::RParen);
                    }
                    return Pattern { kind: PatternKind::Ctor { path, args }, span: Span::new(start, self.prev_end) };
                }
                self.advance();
                Pattern { kind: PatternKind::Bind(name), span }
            }
            TokenKind::Int(_) | TokenKind::Float(_) | TokenKind::Str(_) | TokenKind::Minus | TokenKind::Kw(crate::token::Kw::True | crate::token::Kw::False | crate::token::Kw::Null) => {
                let lit = self.parse_literal_pattern();
                Pattern { kind: PatternKind::Lit(Box::new(lit)), span: Span::new(start, self.prev_end) }
            }
            TokenKind::LBracket => {
                self.advance();
                let mut items = Vec::new();
                let mut rest = None;
                while !self.is(&TokenKind::RBracket) && !self.is(&TokenKind::Eof) {
                    if self.eat(&TokenKind::Ellipsis) {
                        rest = Some(if matches!(self.cur.kind, TokenKind::Ident(_)) { Some(self.expect_ident("a name")) } else { None });
                    } else {
                        items.push(self.parse_pattern());
                    }
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBracket);
                Pattern { kind: PatternKind::Array { items, rest }, span: Span::new(start, self.prev_end) }
            }
            TokenKind::LBrace => {
                self.advance();
                let mut fields = Vec::new();
                while !self.is(&TokenKind::RBrace) && !self.is(&TokenKind::Eof) {
                    let name = self.expect_ident("a field name");
                    self.expect(&TokenKind::Colon);
                    let p = self.parse_pattern();
                    fields.push((name, p));
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBrace);
                Pattern { kind: PatternKind::Object(fields), span: Span::new(start, self.prev_end) }
            }
            other => {
                let found = other.describe();
                self.error(span, format!("expected a pattern, found {found}"));
                if !matches!(other, TokenKind::Colon | TokenKind::RBrace | TokenKind::Eof) {
                    self.advance();
                }
                Pattern { kind: PatternKind::Wildcard, span }
            }
        }
    }

    fn parse_literal_pattern(&mut self) -> Expr {
        let start = self.cur.span.start;
        let negative = self.eat(&TokenKind::Minus);
        let span = self.cur.span;
        let kind = match self.cur.kind.clone() {
            TokenKind::Int(v) => ExprKind::Int(if negative { v.wrapping_neg() } else { v }),
            TokenKind::Float(v) => ExprKind::Float(if negative { -v } else { v }),
            TokenKind::Str(s) if !negative => ExprKind::Str(s),
            TokenKind::Kw(crate::token::Kw::True) if !negative => ExprKind::Bool(true),
            TokenKind::Kw(crate::token::Kw::False) if !negative => ExprKind::Bool(false),
            TokenKind::Kw(crate::token::Kw::Null) if !negative => ExprKind::Null,
            other => {
                let found = other.describe();
                self.error(span, format!("expected a number after `-`, found {found}"));
                return Expr::new(ExprKind::Error, span);
            }
        };
        self.advance();
        Expr::new(kind, Span::new(start, self.prev_end))
    }
}
