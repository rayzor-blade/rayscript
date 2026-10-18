//! The lexer. It is driven by the parser, one token at a time, so that markup can take over the
//! characters between tags: the parser reads raw bytes there and starts lexing tokens again afterwards.

use crate::span::{Diagnostic, Span};
use crate::token::{AssignOp, Kw, StrPart, Token, TokenKind};

pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    end: usize,
    pub diags: Vec<Diagnostic>,
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Lexer<'a> {
        Lexer { src, bytes: src.as_bytes(), pos: 0, end: src.len(), diags: Vec::new() }
    }

    /// A lexer over `start..end` of the same source, so spans stay absolute.
    pub fn range(src: &'a str, start: usize, end: usize) -> Lexer<'a> {
        Lexer { src, bytes: src.as_bytes(), pos: start, end, diags: Vec::new() }
    }

    pub fn src(&self) -> &'a str {
        self.src
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.end
    }

    /// The byte at the position, if there is one before the end.
    pub fn peek_byte(&self) -> Option<u8> {
        if self.pos < self.end { Some(self.bytes[self.pos]) } else { None }
    }

    pub fn peek_byte_at(&self, ahead: usize) -> Option<u8> {
        if self.pos + ahead < self.end { Some(self.bytes[self.pos + ahead]) } else { None }
    }

    pub fn bump_byte(&mut self) {
        self.pos += 1;
    }

    fn peek_char(&self) -> Option<char> {
        if self.pos < self.end { self.src[self.pos..self.end].chars().next() } else { None }
    }

    fn peek_char_at(&self, pos: usize) -> Option<char> {
        if pos < self.end { self.src[pos..self.end].chars().next() } else { None }
    }

    /// Skips whitespace and comments; says whether a newline was crossed.
    pub fn skip_trivia(&mut self) -> bool {
        let mut newline = false;
        loop {
            match self.peek_byte() {
                Some(b'\n') => {
                    newline = true;
                    self.pos += 1;
                }
                Some(b' ' | b'\t' | b'\r') => self.pos += 1,
                Some(b'/') if self.peek_byte_at(1) == Some(b'/') => {
                    while let Some(b) = self.peek_byte() {
                        if b == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.peek_byte_at(1) == Some(b'*') => {
                    let start = self.pos;
                    self.pos += 2;
                    loop {
                        match self.peek_byte() {
                            None => {
                                self.diags.push(Diagnostic::error("this comment is never closed", Span::new(start as u32, (start + 2) as u32)).with_help("end it with */"));
                                break;
                            }
                            Some(b'*') if self.peek_byte_at(1) == Some(b'/') => {
                                self.pos += 2;
                                break;
                            }
                            Some(b'\n') => {
                                newline = true;
                                self.pos += 1;
                            }
                            Some(_) => self.pos += 1,
                        }
                    }
                }
                _ => return newline,
            }
        }
    }

    /// A name in markup, `[A-Za-z_][A-Za-z0-9_-]*`, with `.` allowed between parts when `dotted`.
    pub fn scan_markup_name(&mut self, dotted: bool) -> Option<(String, Span)> {
        let start = self.pos;
        let first = self.peek_char()?;
        if !is_ident_start(first) {
            return None;
        }
        self.pos += first.len_utf8();
        while let Some(c) = self.peek_char() {
            let part = is_ident_continue(c) || c == '-' || (dotted && c == '.' && self.peek_char_at(self.pos + 1).is_some_and(is_ident_start));
            if !part {
                break;
            }
            self.pos += c.len_utf8();
        }
        Some((self.src[start..self.pos].to_string(), Span::new(start as u32, self.pos as u32)))
    }

    /// Text between tags: up to the next `<` or `{`, with `&lt; &gt; &amp; &lbrace; &rbrace;` read as the characters.
    pub fn scan_markup_text(&mut self) -> Option<(String, Span)> {
        let start = self.pos;
        let mut out = String::new();
        while let Some(c) = self.peek_char() {
            if c == '<' || c == '{' {
                break;
            }
            if c == '&' {
                let rest = &self.src[self.pos..self.end];
                let mut matched = false;
                for (entity, text) in [("&lt;", "<"), ("&gt;", ">"), ("&amp;", "&"), ("&lbrace;", "{"), ("&rbrace;", "}"), ("&quot;", "\""), ("&nbsp;", "\u{a0}")] {
                    if rest.starts_with(entity) {
                        out.push_str(text);
                        self.pos += entity.len();
                        matched = true;
                        break;
                    }
                }
                if matched {
                    continue;
                }
            }
            out.push(c);
            self.pos += c.len_utf8();
        }
        if self.pos == start { None } else { Some((out, Span::new(start as u32, self.pos as u32))) }
    }

    pub fn next_token(&mut self) -> Token {
        let newline_before = self.skip_trivia();
        let start = self.pos;
        let kind = self.scan(start);
        Token { kind, span: Span::new(start as u32, self.pos as u32), newline_before }
    }

    fn error(&mut self, message: impl Into<String>, start: usize, end: usize) {
        self.diags.push(Diagnostic::error(message, Span::new(start as u32, end as u32)));
    }

    fn scan(&mut self, start: usize) -> TokenKind {
        let Some(c) = self.peek_char() else { return TokenKind::Eof };
        if is_ident_start(c) {
            while let Some(c) = self.peek_char() {
                if !is_ident_continue(c) {
                    break;
                }
                self.pos += c.len_utf8();
            }
            let text = &self.src[start..self.pos];
            return match Kw::from_str(text) {
                Some(k) => TokenKind::Kw(k),
                None => TokenKind::Ident(text.to_string()),
            };
        }
        if c.is_ascii_digit() {
            return self.scan_number(start);
        }
        match c {
            '"' => return self.scan_double(start),
            '\'' => return self.scan_single(start),
            _ => {}
        }
        self.pos += c.len_utf8();
        let next = self.peek_byte();
        let next2 = self.peek_byte_at(1);
        let take = |lexer: &mut Lexer<'a>, n: usize, kind: TokenKind| {
            lexer.pos += n;
            kind
        };
        match c {
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semi,
            ':' => TokenKind::Colon,
            '@' => TokenKind::At,
            '~' => TokenKind::Tilde,
            '.' => {
                if next == Some(b'.') && next2 == Some(b'.') {
                    take(self, 2, TokenKind::Ellipsis)
                } else {
                    TokenKind::Dot
                }
            }
            '?' => {
                if next == Some(b'?') {
                    if next2 == Some(b'=') { take(self, 2, TokenKind::AssignOp(AssignOp::Coalesce)) } else { take(self, 1, TokenKind::Coalesce) }
                } else if next == Some(b'.') && !next2.is_some_and(|b| b.is_ascii_digit()) {
                    take(self, 1, TokenKind::SafeDot)
                } else {
                    TokenKind::Question
                }
            }
            '+' => match next {
                Some(b'+') => take(self, 1, TokenKind::PlusPlus),
                Some(b'=') => take(self, 1, TokenKind::AssignOp(AssignOp::Add)),
                _ => TokenKind::Plus,
            },
            '-' => match next {
                Some(b'-') => take(self, 1, TokenKind::MinusMinus),
                Some(b'=') => take(self, 1, TokenKind::AssignOp(AssignOp::Sub)),
                Some(b'>') => take(self, 1, TokenKind::Arrow),
                _ => TokenKind::Minus,
            },
            '*' => if next == Some(b'=') { take(self, 1, TokenKind::AssignOp(AssignOp::Mul)) } else { TokenKind::Star },
            '/' => if next == Some(b'=') { take(self, 1, TokenKind::AssignOp(AssignOp::Div)) } else { TokenKind::Slash },
            '%' => if next == Some(b'=') { take(self, 1, TokenKind::AssignOp(AssignOp::Rem)) } else { TokenKind::Percent },
            '^' => if next == Some(b'=') { take(self, 1, TokenKind::AssignOp(AssignOp::Xor)) } else { TokenKind::Caret },
            '&' => match next {
                Some(b'&') => take(self, 1, TokenKind::AmpAmp),
                Some(b'=') => take(self, 1, TokenKind::AssignOp(AssignOp::And)),
                _ => TokenKind::Amp,
            },
            '|' => match next {
                Some(b'|') => take(self, 1, TokenKind::PipePipe),
                Some(b'=') => take(self, 1, TokenKind::AssignOp(AssignOp::Or)),
                _ => TokenKind::Pipe,
            },
            '!' => if next == Some(b'=') { take(self, 1, TokenKind::NotEq) } else { TokenKind::Bang },
            '=' => match next {
                Some(b'=') => take(self, 1, TokenKind::EqEq),
                Some(b'>') => take(self, 1, TokenKind::FatArrow),
                _ => TokenKind::Assign,
            },
            '<' => match next {
                Some(b'<') => {
                    if next2 == Some(b'=') { take(self, 2, TokenKind::AssignOp(AssignOp::Shl)) } else { take(self, 1, TokenKind::Shl) }
                }
                Some(b'=') => take(self, 1, TokenKind::Le),
                _ => TokenKind::Lt,
            },
            '>' => match next {
                Some(b'>') => {
                    if next2 == Some(b'>') {
                        if self.peek_byte_at(2) == Some(b'=') { take(self, 3, TokenKind::AssignOp(AssignOp::UShr)) } else { take(self, 2, TokenKind::UShr) }
                    } else if next2 == Some(b'=') {
                        take(self, 2, TokenKind::AssignOp(AssignOp::Shr))
                    } else {
                        take(self, 1, TokenKind::Shr)
                    }
                }
                Some(b'=') => take(self, 1, TokenKind::Ge),
                _ => TokenKind::Gt,
            },
            other => {
                self.error(format!("`{other}` is not part of the language"), start, self.pos);
                TokenKind::Error
            }
        }
    }

    fn scan_number(&mut self, start: usize) -> TokenKind {
        if self.peek_byte() == Some(b'0') && matches!(self.peek_byte_at(1), Some(b'x' | b'X')) {
            self.pos += 2;
            let digits_start = self.pos;
            while self.peek_byte().is_some_and(|b| b.is_ascii_hexdigit() || b == b'_') {
                self.pos += 1;
            }
            let digits: String = self.src[digits_start..self.pos].chars().filter(|c| *c != '_').collect();
            return match i64::from_str_radix(&digits, 16) {
                Ok(v) => TokenKind::Int(v),
                Err(_) => {
                    self.error("this hexadecimal number is empty or too large", start, self.pos);
                    TokenKind::Int(0)
                }
            };
        }
        while self.peek_byte().is_some_and(|b| b.is_ascii_digit() || b == b'_') {
            self.pos += 1;
        }
        let mut float = false;
        if self.peek_byte() == Some(b'.') && self.peek_byte_at(1).is_some_and(|b| b.is_ascii_digit()) {
            float = true;
            self.pos += 1;
            while self.peek_byte().is_some_and(|b| b.is_ascii_digit() || b == b'_') {
                self.pos += 1;
            }
        }
        if matches!(self.peek_byte(), Some(b'e' | b'E')) {
            let sign = matches!(self.peek_byte_at(1), Some(b'+' | b'-'));
            let digit_at = if sign { 2 } else { 1 };
            if self.peek_byte_at(digit_at).is_some_and(|b| b.is_ascii_digit()) {
                float = true;
                self.pos += digit_at;
                while self.peek_byte().is_some_and(|b| b.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        let text: String = self.src[start..self.pos].chars().filter(|c| *c != '_').collect();
        if float {
            TokenKind::Float(text.parse().unwrap_or(0.0))
        } else {
            match text.parse::<i64>() {
                Ok(v) => TokenKind::Int(v),
                Err(_) => TokenKind::Float(text.parse().unwrap_or(0.0)),
            }
        }
    }

    /// Reads one escape after a backslash into `out`.
    fn escape(&mut self, out: &mut String, quote: char) {
        let at = self.pos - 1;
        let Some(c) = self.peek_char() else { return };
        self.pos += c.len_utf8();
        match c {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            '$' => out.push('$'),
            'x' => {
                let digits: String = self.src[self.pos..self.end.min(self.pos + 2)].to_string();
                match u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32) {
                    Some(ch) if digits.len() == 2 => {
                        out.push(ch);
                        self.pos += 2;
                    }
                    _ => self.error("`\\x` needs two hexadecimal digits", at, self.pos),
                }
            }
            'u' => {
                if self.peek_byte() == Some(b'{') {
                    let close = self.src[self.pos..self.end].find('}');
                    if let Some(close) = close {
                        let digits = &self.src[self.pos + 1..self.pos + close];
                        match u32::from_str_radix(digits, 16).ok().and_then(char::from_u32) {
                            Some(ch) => out.push(ch),
                            None => self.error("this is not a Unicode code point", at, self.pos + close + 1),
                        }
                        self.pos += close + 1;
                        return;
                    }
                }
                let digits: String = self.src[self.pos..self.end.min(self.pos + 4)].to_string();
                match u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32) {
                    Some(ch) if digits.len() == 4 => {
                        out.push(ch);
                        self.pos += 4;
                    }
                    _ => self.error("`\\u` needs four hexadecimal digits, or {digits}", at, self.pos),
                }
            }
            other => {
                self.error(format!("`\\{other}` is not an escape"), at, self.pos);
                out.push(other);
                let _ = quote;
            }
        }
    }

    fn scan_double(&mut self, start: usize) -> TokenKind {
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek_char() {
                None => {
                    self.error("this string is never closed", start, start + 1);
                    break;
                }
                Some('"') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    self.pos += 1;
                    self.escape(&mut out, '"');
                }
                Some(c) => {
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
        TokenKind::Str(out)
    }

    fn scan_single(&mut self, start: usize) -> TokenKind {
        self.pos += 1;
        let mut parts: Vec<StrPart> = Vec::new();
        let mut text = String::new();
        let mut interpolated = false;
        loop {
            match self.peek_char() {
                None => {
                    self.error("this string is never closed", start, start + 1);
                    break;
                }
                Some('\'') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    self.pos += 1;
                    self.escape(&mut text, '\'');
                }
                Some('$') => {
                    let next = self.peek_char_at(self.pos + 1);
                    match next {
                        Some('$') => {
                            text.push('$');
                            self.pos += 2;
                        }
                        Some('{') => {
                            let open = self.pos + 1;
                            match self.skip_braced(open) {
                                Some(close) => {
                                    if !text.is_empty() {
                                        parts.push(StrPart::Lit(std::mem::take(&mut text)));
                                    }
                                    parts.push(StrPart::Expr(Span::new((open + 1) as u32, close as u32)));
                                    interpolated = true;
                                    self.pos = close + 1;
                                }
                                None => {
                                    self.error("this `${` is never closed", self.pos, self.pos + 2);
                                    self.pos += 2;
                                }
                            }
                        }
                        Some(c) if is_ident_start(c) => {
                            let name_start = self.pos + 1;
                            let mut p = name_start;
                            while let Some(c) = self.peek_char_at(p) {
                                if !is_ident_continue(c) {
                                    break;
                                }
                                p += c.len_utf8();
                            }
                            if !text.is_empty() {
                                parts.push(StrPart::Lit(std::mem::take(&mut text)));
                            }
                            parts.push(StrPart::Ident(self.src[name_start..p].to_string(), Span::new(name_start as u32, p as u32)));
                            interpolated = true;
                            self.pos = p;
                        }
                        _ => {
                            text.push('$');
                            self.pos += 1;
                        }
                    }
                }
                Some(c) => {
                    text.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
        if !interpolated {
            return TokenKind::Str(text);
        }
        if !text.is_empty() {
            parts.push(StrPart::Lit(text));
        }
        TokenKind::Interp(parts)
    }

    /// From the `{` at `open`, the position of its matching `}`, skipping strings and nested braces.
    fn skip_braced(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut p = open;
        while p < self.end {
            let c = self.peek_char_at(p)?;
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(p);
                    }
                }
                '"' | '\'' => {
                    let quote = c;
                    p += 1;
                    while p < self.end {
                        let d = self.peek_char_at(p)?;
                        if d == '\\' {
                            p += 1 + self.peek_char_at(p + 1).map(|e| e.len_utf8()).unwrap_or(0);
                            continue;
                        }
                        if quote == '\'' && d == '$' && self.peek_char_at(p + 1) == Some('{') {
                            p = self.skip_braced(p + 1)? + 1;
                            continue;
                        }
                        if d == quote {
                            break;
                        }
                        p += d.len_utf8();
                    }
                }
                _ => {}
            }
            p += c.len_utf8();
        }
        None
    }
}
