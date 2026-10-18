use super::*;
use crate::vm::ops;
use std::cell::Cell;

thread_local! {
    static RNG: Cell<u64> = const { Cell::new(0) };
}

/// A float in [0, 1): xorshift64*, seeded from the clock the first time.
fn random() -> f64 {
    RNG.with(|r| {
        let mut x = r.get();
        if x == 0 {
            x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9E3779B97F4A7C15) | 1;
        }
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        r.set(x);
        (x.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    })
}

fn num(vm: &mut Vm, args: &[Value], what: &str) -> Result<f64, Value> {
    float_arg(vm, args, 0, what)
}

fn f1(vm: &mut Vm, args: &[Value], what: &str, f: fn(f64) -> f64) -> NativeResult {
    let x = num(vm, args, what)?;
    Ok(Value::Float(f(x)))
}

/// A whole-number result as an Int, as Haxe's `Math.floor` and its kin give.
fn int_of(f: f64) -> Value {
    if f.is_finite() && f.abs() < 9.0e18 { Value::Int(f as i64) } else { Value::Float(f) }
}

pub fn install(vm: &mut Vm) {
    // Std
    vm.define_module(
        "Std",
        &[
            ("string", |vm, args| {
                let s = vm.display(args.first().copied().unwrap_or(Value::Nil));
                Ok(vm.new_string(&s))
            }),
            ("int", |vm, args| Ok(int_of(num(vm, args, "Std.int")?.trunc()))),
            ("parseInt", |vm, args| {
                let t = str_arg(vm, args, 0, "Std.parseInt")?.trim();
                let (neg, digits) = match t.strip_prefix('-') {
                    Some(r) => (true, r),
                    None => (false, t.strip_prefix('+').unwrap_or(t)),
                };
                let v = if let Some(h) = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
                    i64::from_str_radix(h.split(|c: char| !c.is_ascii_hexdigit()).next().unwrap_or(""), 16).ok()
                } else {
                    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
                    digits[..end].parse::<i64>().ok()
                };
                Ok(v.map(|v| Value::Int(if neg { -v } else { v })).unwrap_or(Value::Nil))
            }),
            ("parseFloat", |vm, args| {
                let t = str_arg(vm, args, 0, "Std.parseFloat")?.trim();
                let mut end = 0;
                for (i, _) in t.char_indices().chain(std::iter::once((t.len(), ' '))) {
                    if t[..i].parse::<f64>().is_ok() {
                        end = i;
                    }
                }
                Ok(t[..end].parse::<f64>().map(Value::Float).unwrap_or(Value::Float(f64::NAN)))
            }),
            ("random", |vm, args| {
                let n = int_arg(vm, args, 0, "Std.random")?;
                Ok(Value::Int(if n <= 0 { 0 } else { (random() * n as f64) as i64 }))
            }),
            ("isOfType", |vm, args| {
                let name = str_arg(vm, args, 1, "Std.isOfType")?;
                Ok(Value::Bool(vm.is_type(args[0], name)))
            }),
        ],
    );
    // Math
    vm.define_module(
        "Math",
        &[
            ("floor", |vm, args| Ok(int_of(num(vm, args, "Math.floor")?.floor()))),
            ("ceil", |vm, args| Ok(int_of(num(vm, args, "Math.ceil")?.ceil()))),
            ("round", |vm, args| Ok(int_of((num(vm, args, "Math.round")? + 0.5).floor()))),
            ("abs", |vm, args| match args.first() {
                Some(Value::Int(i)) => Ok(Value::Int(i.wrapping_abs())),
                _ => Ok(Value::Float(num(vm, args, "Math.abs")?.abs())),
            }),
            ("sqrt", |vm, args| f1(vm, args, "Math.sqrt", f64::sqrt)),
            ("sin", |vm, args| f1(vm, args, "Math.sin", f64::sin)),
            ("cos", |vm, args| f1(vm, args, "Math.cos", f64::cos)),
            ("tan", |vm, args| f1(vm, args, "Math.tan", f64::tan)),
            ("asin", |vm, args| f1(vm, args, "Math.asin", f64::asin)),
            ("acos", |vm, args| f1(vm, args, "Math.acos", f64::acos)),
            ("atan", |vm, args| f1(vm, args, "Math.atan", f64::atan)),
            ("exp", |vm, args| f1(vm, args, "Math.exp", f64::exp)),
            ("log", |vm, args| f1(vm, args, "Math.log", f64::ln)),
            ("atan2", |vm, args| {
                let y = float_arg(vm, args, 0, "Math.atan2")?;
                let x = float_arg(vm, args, 1, "Math.atan2")?;
                Ok(Value::Float(y.atan2(x)))
            }),
            ("pow", |vm, args| {
                let a = float_arg(vm, args, 0, "Math.pow")?;
                let b = float_arg(vm, args, 1, "Math.pow")?;
                Ok(Value::Float(a.powf(b)))
            }),
            ("min", |vm, args| {
                let (a, b) = (float_arg(vm, args, 0, "Math.min")?, float_arg(vm, args, 1, "Math.min")?);
                Ok(if let (Value::Int(x), Value::Int(y)) = (args[0], args[1]) { Value::Int(x.min(y)) } else { Value::Float(a.min(b)) })
            }),
            ("max", |vm, args| {
                let (a, b) = (float_arg(vm, args, 0, "Math.max")?, float_arg(vm, args, 1, "Math.max")?);
                Ok(if let (Value::Int(x), Value::Int(y)) = (args[0], args[1]) { Value::Int(x.max(y)) } else { Value::Float(a.max(b)) })
            }),
            ("random", |_, _| Ok(Value::Float(random()))),
            ("isNaN", |vm, args| Ok(Value::Bool(num(vm, args, "Math.isNaN")?.is_nan()))),
            ("isFinite", |vm, args| Ok(Value::Bool(num(vm, args, "Math.isFinite")?.is_finite()))),
        ],
    );
    if let Some(math) = vm.global("Math") {
        for (name, v) in [("PI", std::f64::consts::PI), ("NaN", f64::NAN), ("POSITIVE_INFINITY", f64::INFINITY), ("NEGATIVE_INFINITY", f64::NEG_INFINITY)] {
            vm.set_member(math, name, Value::Float(v));
        }
    }
    // StringTools
    vm.define_module(
        "StringTools",
        &[
            ("trim", |vm, args| {
                let s = str_arg(vm, args, 0, "StringTools.trim")?;
                new_str(vm, s.trim())
            }),
            ("ltrim", |vm, args| {
                let s = str_arg(vm, args, 0, "StringTools.ltrim")?;
                new_str(vm, s.trim_start())
            }),
            ("rtrim", |vm, args| {
                let s = str_arg(vm, args, 0, "StringTools.rtrim")?;
                new_str(vm, s.trim_end())
            }),
            ("startsWith", |vm, args| {
                let (a, b) = (str_arg(vm, args, 0, "StringTools.startsWith")?, str_arg(vm, args, 1, "StringTools.startsWith")?);
                Ok(Value::Bool(a.starts_with(b)))
            }),
            ("endsWith", |vm, args| {
                let (a, b) = (str_arg(vm, args, 0, "StringTools.endsWith")?, str_arg(vm, args, 1, "StringTools.endsWith")?);
                Ok(Value::Bool(a.ends_with(b)))
            }),
            ("contains", |vm, args| {
                let (a, b) = (str_arg(vm, args, 0, "StringTools.contains")?, str_arg(vm, args, 1, "StringTools.contains")?);
                Ok(Value::Bool(a.contains(b)))
            }),
            ("replace", |vm, args| {
                let (a, b, c) = (str_arg(vm, args, 0, "StringTools.replace")?, str_arg(vm, args, 1, "StringTools.replace")?, str_arg(vm, args, 2, "StringTools.replace")?);
                new_str(vm, &a.replace(b, c))
            }),
            ("lpad", |vm, args| {
                let (s, c, l) = (str_arg(vm, args, 0, "StringTools.lpad")?, str_arg(vm, args, 1, "StringTools.lpad")?, int_arg(vm, args, 2, "StringTools.lpad")?);
                let fill = c.chars().next().unwrap_or(' ');
                let n = s.chars().count() as i64;
                new_str(vm, &format!("{}{s}", fill.to_string().repeat((l - n).max(0) as usize)))
            }),
            ("rpad", |vm, args| {
                let (s, c, l) = (str_arg(vm, args, 0, "StringTools.rpad")?, str_arg(vm, args, 1, "StringTools.rpad")?, int_arg(vm, args, 2, "StringTools.rpad")?);
                let fill = c.chars().next().unwrap_or(' ');
                let n = s.chars().count() as i64;
                new_str(vm, &format!("{s}{}", fill.to_string().repeat((l - n).max(0) as usize)))
            }),
            ("hex", |vm, args| {
                let n = int_arg(vm, args, 0, "StringTools.hex")?;
                let digits = opt_int(args, 1).unwrap_or(0).max(0) as usize;
                new_str(vm, &format!("{:0width$X}", n as u64 & 0xFFFF_FFFF, width = digits))
            }),
        ],
    );
    // Lambda
    vm.define_module(
        "Lambda",
        &[
            ("array", |vm, args| {
                let items = elements(vm, args.first().copied().unwrap_or(Value::Nil))?;
                Ok(vm.new_array(items))
            }),
            ("map", |vm, args| {
                let items = elements(vm, args.first().copied().unwrap_or(Value::Nil))?;
                let f = args.get(1).copied().unwrap_or(Value::Nil);
                let mut out = Vec::with_capacity(items.len());
                for x in items {
                    out.push(vm.call(f, &[x])?);
                }
                Ok(vm.new_array(out))
            }),
            ("filter", |vm, args| {
                let items = elements(vm, args.first().copied().unwrap_or(Value::Nil))?;
                let f = args.get(1).copied().unwrap_or(Value::Nil);
                let mut out = Vec::new();
                for x in items {
                    if vm.call(f, &[x])?.truthy() {
                        out.push(x);
                    }
                }
                Ok(vm.new_array(out))
            }),
            ("fold", |vm, args| {
                let items = elements(vm, args.first().copied().unwrap_or(Value::Nil))?;
                let f = args.get(1).copied().unwrap_or(Value::Nil);
                let mut acc = args.get(2).copied().unwrap_or(Value::Nil);
                for x in items {
                    acc = vm.call(f, &[x, acc])?;
                }
                Ok(acc)
            }),
            ("iter", |vm, args| {
                let items = elements(vm, args.first().copied().unwrap_or(Value::Nil))?;
                let f = args.get(1).copied().unwrap_or(Value::Nil);
                for x in items {
                    vm.call(f, &[x])?;
                }
                Ok(Value::Nil)
            }),
            ("exists", |vm, args| {
                let items = elements(vm, args.first().copied().unwrap_or(Value::Nil))?;
                let f = args.get(1).copied().unwrap_or(Value::Nil);
                for x in items {
                    if vm.call(f, &[x])?.truthy() {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }),
            ("find", |vm, args| {
                let items = elements(vm, args.first().copied().unwrap_or(Value::Nil))?;
                let f = args.get(1).copied().unwrap_or(Value::Nil);
                for x in items {
                    if vm.call(f, &[x])?.truthy() {
                        return Ok(x);
                    }
                }
                Ok(Value::Nil)
            }),
            ("count", |vm, args| {
                let items = elements(vm, args.first().copied().unwrap_or(Value::Nil))?;
                match args.get(1).copied() {
                    Some(f) if !f.is_nil() => {
                        let mut n = 0;
                        for x in items {
                            if vm.call(f, &[x])?.truthy() {
                                n += 1;
                            }
                        }
                        Ok(Value::Int(n))
                    }
                    _ => Ok(Value::Int(items.len() as i64)),
                }
            }),
            ("empty", |vm, args| Ok(Value::Bool(elements(vm, args.first().copied().unwrap_or(Value::Nil))?.is_empty()))),
            ("has", |vm, args| {
                let items = elements(vm, args.first().copied().unwrap_or(Value::Nil))?;
                let x = args.get(1).copied().unwrap_or(Value::Nil);
                Ok(Value::Bool(items.iter().any(|v| ops::equal(*v, x))))
            }),
        ],
    );
    // Reflect
    vm.define_module(
        "Reflect",
        &[
            ("field", |vm, args| {
                let name = str_arg(vm, args, 1, "Reflect.field")?;
                match args[0] {
                    Value::Obj(o) if o.kind() == Kind::Object => {
                        let sym = vm.intern(name);
                        Ok(o.object().fields.get(sym).unwrap_or(Value::Nil))
                    }
                    _ => Ok(Value::Nil),
                }
            }),
            ("setField", |vm, args| {
                let name = str_arg(vm, args, 1, "Reflect.setField")?;
                let sym = vm.intern(name);
                vm.set_field(args[0], sym, args.get(2).copied().unwrap_or(Value::Nil))?;
                Ok(Value::Nil)
            }),
            ("hasField", |vm, args| {
                let name = str_arg(vm, args, 1, "Reflect.hasField")?;
                let sym = vm.intern(name);
                Ok(Value::Bool(matches!(args[0], Value::Obj(o) if o.kind() == Kind::Object && o.object().fields.has(sym))))
            }),
            ("deleteField", |vm, args| {
                let name = str_arg(vm, args, 1, "Reflect.deleteField")?;
                let sym = vm.intern(name);
                Ok(Value::Bool(matches!(args[0], Value::Obj(o) if o.kind() == Kind::Object && o.object().fields.remove(sym))))
            }),
            ("fields", |vm, args| {
                let names: Vec<String> = match args[0] {
                    Value::Obj(o) if o.kind() == Kind::Object => o.object().fields.iter().map(|(s, _)| vm.syms.name(*s).to_string()).collect(),
                    _ => Vec::new(),
                };
                let items = names.iter().map(|n| vm.new_string(n)).collect();
                Ok(vm.new_array(items))
            }),
            ("isFunction", |_, args| Ok(Value::Bool(matches!(args[0], Value::Native(_)) || matches!(args[0], Value::Obj(o) if o.kind() == Kind::Closure)))),
            ("isObject", |_, args| Ok(Value::Bool(matches!(args[0], Value::Obj(o) if o.kind() == Kind::Object)))),
            ("callMethod", |vm, args| {
                let f = args.get(1).copied().unwrap_or(Value::Nil);
                let a = match args.get(2) {
                    Some(Value::Obj(o)) if o.kind() == Kind::Array => o.array().items.clone(),
                    _ => Vec::new(),
                };
                vm.call(f, &a)
            }),
            ("copy", |vm, args| match args[0] {
                Value::Obj(o) if o.kind() == Kind::Object => {
                    let out = vm.new_object();
                    let entries: Vec<(u32, Value)> = o.object().fields.iter().copied().collect();
                    for (k, v) in entries {
                        out.as_obj().unwrap().object().fields.set(k, v);
                    }
                    Ok(out)
                }
                other => Ok(other),
            }),
        ],
    );
}
