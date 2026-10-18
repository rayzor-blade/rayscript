//! Type annotations: parsed for tools, not enforced when a script runs.

use super::Parser;
use crate::ast::*;
use crate::span::Span;
use crate::token::{AssignOp, TokenKind};

impl<'a> Parser<'a> {
    pub(crate) fn parse_type(&mut self) -> Type {
        let start = self.cur.span.start;
        if self.eat(&TokenKind::Question) {
            let inner = self.parse_type();
            return Type { kind: TypeKind::Optional(Box::new(inner)), span: Span::new(start, self.prev_end) };
        }
        let first = self.parse_type_atom();
        if self.is(&TokenKind::Arrow) {
            self.advance();
            let ret = self.parse_type();
            // `(A, B) -> R` parsed its parenthesised part as a tuple of arguments.
            let args = match first.kind {
                TypeKind::Func { args, ret: tuple } if matches!(tuple.kind, TypeKind::Anon(ref f) if f.is_empty()) => args,
                _ => vec![first],
            };
            return Type { kind: TypeKind::Func { args, ret: Box::new(ret) }, span: Span::new(start, self.prev_end) };
        }
        first
    }

    fn parse_type_atom(&mut self) -> Type {
        let start = self.cur.span.start;
        match &self.cur.kind {
            TokenKind::LParen => {
                // `(A, B)` before `->`: the arguments of a function type.
                self.advance();
                let mut args = Vec::new();
                while !self.is(&TokenKind::RParen) && !self.is(&TokenKind::Eof) {
                    args.push(self.parse_type());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen);
                let span = Span::new(start, self.prev_end);
                if self.is(&TokenKind::Arrow) {
                    return Type { kind: TypeKind::Func { args, ret: Box::new(Type { kind: TypeKind::Anon(Vec::new()), span }) }, span };
                }
                match args.len() {
                    1 => args.pop().unwrap(),
                    _ => {
                        self.error(span, "a parenthesised type needs `->` after it");
                        Type { kind: TypeKind::Anon(Vec::new()), span }
                    }
                }
            }
            TokenKind::LBrace => {
                self.advance();
                let mut fields = Vec::new();
                while !self.is(&TokenKind::RBrace) && !self.is(&TokenKind::Eof) {
                    let optional = self.eat(&TokenKind::Question);
                    let name = self.expect_ident("a field name");
                    self.expect(&TokenKind::Colon);
                    let ty = self.parse_type();
                    fields.push(TypeField { name, ty, optional });
                    if !self.eat(&TokenKind::Comma) && !self.eat(&TokenKind::Semi) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBrace);
                Type { kind: TypeKind::Anon(fields), span: Span::new(start, self.prev_end) }
            }
            _ => {
                let mut path = vec![self.expect_ident("a type name")];
                while self.eat(&TokenKind::Dot) {
                    path.push(self.expect_ident("a name"));
                }
                let mut params = Vec::new();
                if self.eat(&TokenKind::Lt) {
                    loop {
                        params.push(self.parse_type());
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect_type_gt();
                }
                Type { kind: TypeKind::Path { path, params }, span: Span::new(start, self.prev_end) }
            }
        }
    }

    /// The `>` that closes type parameters: `>>`, `>=` and the like are split, the rest being left to read.
    fn expect_type_gt(&mut self) {
        let rest = match self.cur.kind {
            TokenKind::Gt => {
                self.advance();
                return;
            }
            TokenKind::Shr => Some(TokenKind::Gt),
            TokenKind::UShr => Some(TokenKind::Shr),
            TokenKind::Ge => Some(TokenKind::Assign),
            TokenKind::AssignOp(AssignOp::Shr) => Some(TokenKind::Ge),
            TokenKind::AssignOp(AssignOp::UShr) => Some(TokenKind::AssignOp(AssignOp::Shr)),
            _ => None,
        };
        match rest {
            Some(rest) => {
                self.prev_end = self.cur.span.start + 1;
                self.cur.span.start += 1;
                self.cur.kind = rest;
            }
            None => {
                let found = self.cur.kind.describe();
                let span = self.cur.span;
                self.error(span, format!("expected `>` to end the type parameters, found {found}"));
            }
        }
    }
}
