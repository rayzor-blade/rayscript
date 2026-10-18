//! `haxe.Json`: `parse` and `stringify` over the VM's values.

use super::*;
use crate::vm::ops;

struct Reader<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Reader<'a> {
    fn ws(&mut self) {
        while self.text[self.pos..].starts_with([' ', '\n', '\t', '\r']) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn fail<T>(&self, vm: &mut Vm, what: &str) -> Result<T, Value> {
        throw(vm, format!("invalid JSON at position {}: {what}", self.pos))
    }

    fn value(&mut self, vm: &mut Vm, depth: usize) -> NativeResult {
        if depth > 200 {
            return self.fail(vm, "nested too deeply");
        }
        self.ws();
        match self.peek() {
            None => self.fail(vm, "the text ends here"),
            Some('{') => {
                self.pos += 1;
                let obj = vm.new_object();
                self.ws();
                if self.peek() == Some('}') {
                    self.pos += 1;
                    return Ok(obj);
                }
                loop {
                    self.ws();
                    let key = match self.value(vm, depth + 1)? {
                        Value::Obj(o) if o.kind() == Kind::String => o.str().to_string(),
                        _ => return self.fail(vm, "an object key must be a string"),
                    };
                    self.ws();
                    if self.peek() != Some(':') {
                        return self.fail(vm, "expected `:`");
                    }
                    self.pos += 1;
                    let v = self.value(vm, depth + 1)?;
                    let sym = vm.intern(&key);
                    obj.as_obj().unwrap().object().fields.set(sym, v);
                    self.ws();
                    match self.peek() {
                        Some(',') => self.pos += 1,
                        Some('}') => {
                            self.pos += 1;
                            return Ok(obj);
                        }
                        _ => return self.fail(vm, "expected `,` or `}`"),
                    }
                }
            }
            Some('[') => {
                self.pos += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(']') {
                    self.pos += 1;
                    return Ok(vm.new_array(items));
                }
                loop {
                    items.push(self.value(vm, depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(',') => self.pos += 1,
                        Some(']') => {
                            self.pos += 1;
                            return Ok(vm.new_array(items));
                        }
                        _ => return self.fail(vm, "expected `,` or `]`"),
                    }
                }
            }
            Some('"') => {
                self.pos += 1;
                let mut out = String::new();
                loop {
                    let Some(c) = self.peek() else { return self.fail(vm, "the string is never closed") };
                    self.pos += c.len_utf8();
                    match c {
                        '"' => return Ok(vm.new_string(&out)),
                        '\\' => {
                            let Some(e) = self.peek() else { return self.fail(vm, "the escape is cut off") };
                            self.pos += e.len_utf8();
                            match e {
                                'n' => out.push('\n'),
                                't' => out.push('\t'),
                                'r' => out.push('\r'),
                                'b' => out.push('\u{8}'),
                                'f' => out.push('\u{c}'),
                                '/' | '\\' | '"' => out.push(e),
                                'u' => {
                                    let hex = self.text.get(self.pos..self.pos + 4).unwrap_or("");
                                    match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                                        Some(ch) => out.push(ch),
                                        None => return self.fail(vm, "a bad \\u escape"),
                                    }
                                    self.pos += 4;
                                }
                                _ => return self.fail(vm, "an unknown escape"),
                            }
                        }
                        c => out.push(c),
                    }
                }
            }
            Some(_) => {
                for (word, v) in [("true", Value::Bool(true)), ("false", Value::Bool(false)), ("null", Value::Nil)] {
                    if self.text[self.pos..].starts_with(word) {
                        self.pos += word.len();
                        return Ok(v);
                    }
                }
                let rest = &self.text[self.pos..];
                let end = rest.find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))).unwrap_or(rest.len());
                if end == 0 {
                    return self.fail(vm, "an unexpected character");
                }
                let n = &rest[..end];
                self.pos += end;
                match n.parse::<i64>() {
                    Ok(i) => Ok(Value::Int(i)),
                    Err(_) => match n.parse::<f64>() {
                        Ok(f) => Ok(Value::Float(f)),
                        Err(_) => self.fail(vm, "a bad number"),
                    },
                }
            }
        }
    }
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write(vm: &mut Vm, v: Value, indent: &str, level: usize, out: &mut String) -> Result<(), Value> {
    if level > 100 {
        return throw(vm, "cannot make JSON of something that contains itself");
    }
    let nl = |out: &mut String, lvl: usize| {
        if !indent.is_empty() {
            out.push('\n');
            out.push_str(&indent.repeat(lvl));
        }
    };
    match v {
        Value::Nil => out.push_str("null"),
        Value::Bool(b) => out.push_str(&b.to_string()),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => out.push_str(&if f.is_finite() { format!("{f}") } else { "null".into() }),
        Value::Native(_) => out.push_str("null"),
        Value::Obj(o) => match o.kind() {
            Kind::String => quote(o.str(), out),
            Kind::Array => {
                let items = o.array().items.clone();
                if items.is_empty() {
                    out.push_str("[]");
                } else {
                    out.push('[');
                    for (i, x) in items.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        nl(out, level + 1);
                        write(vm, *x, indent, level + 1, out)?;
                    }
                    nl(out, level);
                    out.push(']');
                }
            }
            Kind::Object => {
                let fields: Vec<(u32, Value)> = o.object().fields.iter().copied().collect();
                if fields.is_empty() {
                    out.push_str("{}");
                } else {
                    out.push('{');
                    for (i, (k, x)) in fields.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        nl(out, level + 1);
                        quote(vm.syms.name(*k), out);
                        out.push(':');
                        if !indent.is_empty() {
                            out.push(' ');
                        }
                        write(vm, *x, indent, level + 1, out)?;
                    }
                    nl(out, level);
                    out.push('}');
                }
            }
            Kind::Map => {
                let entries: Vec<(Value, Value)> = o.map().map.iter().collect();
                out.push('{');
                for (i, (k, x)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    nl(out, level + 1);
                    quote(&ops::display(vm, *k, 0), out);
                    out.push(':');
                    write(vm, *x, indent, level + 1, out)?;
                }
                nl(out, level);
                out.push('}');
            }
            _ => out.push_str("null"),
        },
    }
    Ok(())
}

pub fn install(vm: &mut Vm) {
    vm.define_module(
        "haxe.Json",
        &[
            ("parse", |vm, args| {
                let text = str_arg(vm, args, 0, "Json.parse")?;
                let mut r = Reader { text, pos: 0 };
                let v = r.value(vm, 0)?;
                r.ws();
                if r.pos != text.len() {
                    return r.fail(vm, "there is more after the value");
                }
                Ok(v)
            }),
            ("stringify", |vm, args| {
                let indent = match args.get(2) {
                    Some(Value::Obj(o)) if o.kind() == Kind::String => o.str().to_string(),
                    Some(Value::Int(n)) if *n > 0 => " ".repeat(*n as usize),
                    _ => String::new(),
                };
                let mut out = String::new();
                write(vm, args.first().copied().unwrap_or(Value::Nil), &indent, 0, &mut out)?;
                Ok(vm.new_string(&out))
            }),
        ],
    );
    // `Json` alone, as scripts that import haxe.Json name it.
    if let Some(j) = vm.import_value("haxe.Json").ok() {
        vm.define_global("Json", j);
    }
}
