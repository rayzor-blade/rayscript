//! What operators and accesses do to values.

use super::Vm;
use crate::object::{Kind, MapKey};
use crate::value::Value;

pub fn type_name(v: Value) -> &'static str {
    match v {
        Value::Nil => "null",
        Value::Bool(_) => "Bool",
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::Native(_) => "Function",
        Value::Obj(o) => o.kind().name(),
    }
}

/// The key of the method table for values that are not objects.
pub fn kind_tag(v: Value) -> u8 {
    match v {
        Value::Nil => 100,
        Value::Bool(_) => 101,
        Value::Int(_) => 102,
        Value::Float(_) => 103,
        Value::Native(_) => 104,
        Value::Obj(o) => o.kind() as u8,
    }
}

pub fn equal(a: Value, b: Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => x as f64 == y,
        (Value::Native(x), Value::Native(y)) => x == y,
        (Value::Obj(x), Value::Obj(y)) => {
            if x == y {
                return true;
            }
            x.kind() == Kind::String && y.kind() == Kind::String && x.str() == y.str()
        }
        _ => false,
    }
}

fn float_text(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        format!("{f}")
    }
}

const MAX_DISPLAY_DEPTH: usize = 8;

/// A value as text. Strings inside collections are shown as they are, as Haxe does.
pub fn display(vm: &Vm, v: Value, depth: usize) -> String {
    match v {
        Value::Nil => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => float_text(f),
        Value::Native(n) => format!("<function {}>", vm.natives[n as usize].name),
        Value::Obj(o) => match o.kind() {
            Kind::String => o.str().to_string(),
            _ if depth >= MAX_DISPLAY_DEPTH => "...".into(),
            Kind::Array => format!("[{}]", o.array().items.iter().map(|x| display(vm, *x, depth + 1)).collect::<Vec<_>>().join(",")),
            Kind::Object => format!("{{{}}}", o.object().fields.iter().map(|(s, x)| format!("{}: {}", vm.syms.name(*s), display(vm, *x, depth + 1))).collect::<Vec<_>>().join(", ")),
            Kind::Map => format!("{{{}}}", o.map().map.iter().map(|(k, x)| format!("{} => {}", display(vm, k, depth + 1), display(vm, x, depth + 1))).collect::<Vec<_>>().join(", ")),
            Kind::Closure => format!("<function {}>", vm.protos[o.closure().proto as usize].name),
            Kind::Range => format!("{}...{}", o.range().from, o.range().to),
            Kind::Cell => display(vm, o.cell().value, depth + 1),
            Kind::Fiber => format!("<fiber {}>", o.fiber().fiber.id),
            Kind::Promise => "<promise>".into(),
            Kind::Host => "<host object>".into(),
        },
    }
}

impl Vm {
    /// A string to throw: how the VM reports what went wrong.
    pub(crate) fn err(&mut self, message: impl AsRef<str>) -> Value {
        self.new_string(message.as_ref())
    }

    pub(crate) fn add(&mut self, a: Value, b: Value) -> Result<Value, Value> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok(Value::Int(x.wrapping_add(y))),
            (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x + y)),
            (Value::Int(x), Value::Float(y)) => Ok(Value::Float(x as f64 + y)),
            (Value::Float(x), Value::Int(y)) => Ok(Value::Float(x + y as f64)),
            _ => {
                let a_str = matches!(a, Value::Obj(o) if o.kind() == Kind::String);
                let b_str = matches!(b, Value::Obj(o) if o.kind() == Kind::String);
                if a_str || b_str {
                    let mut s = display(self, a, 0);
                    s.push_str(&display(self, b, 0));
                    return Ok(self.new_string(&s));
                }
                Err(self.err(format!("cannot add {} and {}", type_name(a), type_name(b))))
            }
        }
    }

    pub(crate) fn arith(&mut self, op: Arith, a: Value, b: Value) -> Result<Value, Value> {
        use Arith::*;
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok(match op {
                Sub => Value::Int(x.wrapping_sub(y)),
                Mul => Value::Int(x.wrapping_mul(y)),
                Div => {
                    if y == 0 {
                        Value::Float(x as f64 / 0.0)
                    } else {
                        Value::Float(x as f64 / y as f64)
                    }
                }
                Rem => {
                    if y == 0 {
                        return Err(self.err("remainder by zero"));
                    }
                    Value::Int(x.wrapping_rem(y))
                }
                Shl => Value::Int(x.wrapping_shl(y as u32)),
                Shr => Value::Int(x.wrapping_shr(y as u32)),
                UShr => Value::Int(((x as u64).wrapping_shr(y as u32)) as i64),
                BitAnd => Value::Int(x & y),
                BitOr => Value::Int(x | y),
                BitXor => Value::Int(x ^ y),
            }),
            (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
                let x = as_f64(a);
                let y = as_f64(b);
                match op {
                    Sub => Ok(Value::Float(x - y)),
                    Mul => Ok(Value::Float(x * y)),
                    Div => Ok(Value::Float(x / y)),
                    Rem => Ok(Value::Float(x % y)),
                    _ => Err(self.err(format!("`{}` needs integers, not {} and {}", op.symbol(), type_name(a), type_name(b)))),
                }
            }
            _ => Err(self.err(format!("cannot use `{}` on {} and {}", op.symbol(), type_name(a), type_name(b)))),
        }
    }

    pub(crate) fn compare(&mut self, op: Cmp, a: Value, b: Value) -> Result<bool, Value> {
        let ord = match (a, b) {
            (Value::Int(x), Value::Int(y)) => x.partial_cmp(&y),
            (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => as_f64(a).partial_cmp(&as_f64(b)),
            (Value::Obj(x), Value::Obj(y)) if x.kind() == Kind::String && y.kind() == Kind::String => Some(x.str().cmp(y.str())),
            _ => return Err(self.err(format!("cannot compare {} and {}", type_name(a), type_name(b)))),
        };
        Ok(match ord {
            None => false,
            Some(o) => match op {
                Cmp::Lt => o.is_lt(),
                Cmp::Le => o.is_le(),
                Cmp::Gt => o.is_gt(),
                Cmp::Ge => o.is_ge(),
            },
        })
    }

    pub(crate) fn get_field(&mut self, obj: Value, sym: u32) -> Result<Value, Value> {
        if let Value::Obj(o) = obj {
            match o.kind() {
                Kind::Object => return Ok(o.object().fields.get(sym).unwrap_or(Value::Nil)),
                Kind::Array if self.syms.name(sym) == "length" => return Ok(Value::Int(o.array().items.len() as i64)),
                Kind::String if self.syms.name(sym) == "length" => return Ok(Value::Int(o.str().chars().count() as i64)),
                Kind::Map if self.syms.name(sym) == "length" => return Ok(Value::Int(o.map().map.len() as i64)),
                Kind::Range => match self.syms.name(sym) {
                    "from" | "min" => return Ok(Value::Int(o.range().from)),
                    "to" | "max" => return Ok(Value::Int(o.range().to)),
                    _ => {}
                },
                _ => {}
            }
        }
        let name = self.syms.name(sym).to_string();
        Err(self.err(format!("cannot read `{name}` of {}", type_name(obj))))
    }

    pub(crate) fn set_field(&mut self, obj: Value, sym: u32, v: Value) -> Result<(), Value> {
        if let Value::Obj(o) = obj {
            if o.kind() == Kind::Object {
                o.object().fields.set(sym, v);
                return Ok(());
            }
        }
        let name = self.syms.name(sym).to_string();
        Err(self.err(format!("cannot set `{name}` on {}", type_name(obj))))
    }

    pub(crate) fn get_index(&mut self, obj: Value, index: Value) -> Result<Value, Value> {
        if let Value::Obj(o) = obj {
            match (o.kind(), index) {
                (Kind::Array, Value::Int(i)) => {
                    let a = &o.array().items;
                    return Ok(if i >= 0 && (i as usize) < a.len() { a[i as usize] } else { Value::Nil });
                }
                (Kind::String, Value::Int(i)) => {
                    return Ok(match o.str().chars().nth(i.max(0) as usize).filter(|_| i >= 0) {
                        Some(c) => self.new_string(&c.to_string()),
                        None => Value::Nil,
                    });
                }
                (Kind::Map, k) => return Ok(o.map().map.get(k).unwrap_or(Value::Nil)),
                (Kind::Object, Value::Obj(k)) if k.kind() == Kind::String => {
                    let sym = self.syms.intern(k.str());
                    return Ok(o.object().fields.get(sym).unwrap_or(Value::Nil));
                }
                _ => {}
            }
        }
        Err(self.err(format!("cannot index {} with {}", type_name(obj), type_name(index))))
    }

    pub(crate) fn set_index(&mut self, obj: Value, index: Value, v: Value) -> Result<(), Value> {
        if let Value::Obj(o) = obj {
            match (o.kind(), index) {
                (Kind::Array, Value::Int(i)) if i >= 0 => {
                    let a = &mut o.array().items;
                    let i = i as usize;
                    if i >= a.len() {
                        if i > a.len() + 1_000_000 {
                            return Err(self.err("array index is too far past the end"));
                        }
                        a.resize(i + 1, Value::Nil);
                    }
                    a[i] = v;
                    return Ok(());
                }
                (Kind::Map, k) => {
                    o.map().map.set(k, v);
                    return Ok(());
                }
                (Kind::Object, Value::Obj(k)) if k.kind() == Kind::String => {
                    let sym = self.syms.intern(k.str());
                    o.object().fields.set(sym, v);
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(self.err(format!("cannot set an index of {} with {}", type_name(obj), type_name(index))))
    }

    /// Whether a value is of one of the types a `catch` can name.
    pub(crate) fn is_type(&self, v: Value, name: &str) -> bool {
        match (name, v) {
            ("String", Value::Obj(o)) => o.kind() == Kind::String,
            ("Int", Value::Int(_)) => true,
            ("Float", Value::Float(_) | Value::Int(_)) => true,
            ("Bool", Value::Bool(_)) => true,
            ("Array", Value::Obj(o)) => o.kind() == Kind::Array,
            ("Map", Value::Obj(o)) => o.kind() == Kind::Map,
            ("Object", Value::Obj(o)) => o.kind() == Kind::Object,
            _ => false,
        }
    }
}

fn as_f64(v: Value) -> f64 {
    match v {
        Value::Int(i) => i as f64,
        Value::Float(f) => f,
        _ => 0.0,
    }
}

#[derive(Clone, Copy)]
pub enum Arith {
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
}

impl Arith {
    fn symbol(self) -> &'static str {
        match self {
            Arith::Sub => "-",
            Arith::Mul => "*",
            Arith::Div => "/",
            Arith::Rem => "%",
            Arith::Shl => "<<",
            Arith::Shr => ">>",
            Arith::UShr => ">>>",
            Arith::BitAnd => "&",
            Arith::BitOr => "|",
            Arith::BitXor => "^",
        }
    }
}

#[derive(Clone, Copy)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
}

/// Whether a map key of this value would be found by value.
#[allow(dead_code)]
pub fn key_of(v: Value) -> MapKey {
    MapKey::of(v)
}
