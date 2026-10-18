use super::*;

fn s<'a>(args: &[Value]) -> &'a str {
    args[0].as_obj().expect("a method of String is called on a string").str()
}

/// A character position (counting characters, as Haxe does) as a byte offset.
fn byte_at(text: &str, chars: usize) -> usize {
    text.char_indices().nth(chars).map(|(b, _)| b).unwrap_or(text.len())
}

pub fn install(vm: &mut Vm) {
    vm.define_method(Kind::String, "charAt", |vm, args| {
        let i = int_arg(vm, args, 1, "charAt's position")?;
        let c = if i >= 0 { s(args).chars().nth(i as usize) } else { None };
        new_str(vm, &c.map(|c| c.to_string()).unwrap_or_default())
    });
    vm.define_method(Kind::String, "charCodeAt", |vm, args| {
        let i = int_arg(vm, args, 1, "charCodeAt's position")?;
        let c = if i >= 0 { s(args).chars().nth(i as usize) } else { None };
        Ok(c.map(|c| Value::Int(c as i64)).unwrap_or(Value::Nil))
    });
    vm.define_method(Kind::String, "indexOf", |vm, args| {
        let needle = str_arg(vm, args, 1, "indexOf")?;
        let text = s(args);
        let from = byte_at(text, opt_int(args, 2).unwrap_or(0).max(0) as usize);
        Ok(Value::Int(text[from..].find(needle).map(|b| text[..from + b].chars().count() as i64).unwrap_or(-1)))
    });
    vm.define_method(Kind::String, "lastIndexOf", |vm, args| {
        let needle = str_arg(vm, args, 1, "lastIndexOf")?;
        let text = s(args);
        Ok(Value::Int(text.rfind(needle).map(|b| text[..b].chars().count() as i64).unwrap_or(-1)))
    });
    vm.define_method(Kind::String, "split", |vm, args| {
        let sep = str_arg(vm, args, 1, "split")?;
        let text = s(args);
        let parts: Vec<String> = if sep.is_empty() { text.chars().map(|c| c.to_string()).collect() } else { text.split(sep).map(|p| p.to_string()).collect() };
        let items = parts.iter().map(|p| vm.new_string(p)).collect();
        Ok(vm.new_array(items))
    });
    vm.define_method(Kind::String, "substr", |vm, args| {
        let text = s(args);
        let n = text.chars().count() as i64;
        let mut pos = int_arg(vm, args, 1, "substr's position")?;
        if pos < 0 {
            pos = (n + pos).max(0);
        }
        let len = opt_int(args, 2).unwrap_or(n - pos).max(0);
        let start = byte_at(text, pos as usize);
        let end = byte_at(text, (pos + len).min(n) as usize);
        new_str(vm, if start < end { &text[start..end] } else { "" })
    });
    vm.define_method(Kind::String, "substring", |vm, args| {
        let text = s(args);
        let n = text.chars().count() as i64;
        let a = int_arg(vm, args, 1, "substring's start")?.clamp(0, n);
        let b = opt_int(args, 2).unwrap_or(n).clamp(0, n);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        new_str(vm, &text[byte_at(text, lo as usize)..byte_at(text, hi as usize)])
    });
    vm.define_method(Kind::String, "toLowerCase", |vm, args| new_str(vm, &s(args).to_lowercase()));
    vm.define_method(Kind::String, "toUpperCase", |vm, args| new_str(vm, &s(args).to_uppercase()));
    vm.define_method(Kind::String, "toString", |_, args| Ok(args[0]));
    vm.define_method(Kind::String, "trim", |vm, args| new_str(vm, s(args).trim()));
    vm.define_method(Kind::String, "startsWith", |vm, args| {
        let p = str_arg(vm, args, 1, "startsWith")?;
        Ok(Value::Bool(s(args).starts_with(p)))
    });
    vm.define_method(Kind::String, "endsWith", |vm, args| {
        let p = str_arg(vm, args, 1, "endsWith")?;
        Ok(Value::Bool(s(args).ends_with(p)))
    });
    vm.define_method(Kind::String, "contains", |vm, args| {
        let p = str_arg(vm, args, 1, "contains")?;
        Ok(Value::Bool(s(args).contains(p)))
    });
    vm.define_method(Kind::String, "replace", |vm, args| {
        let from = str_arg(vm, args, 1, "replace")?;
        let to = str_arg(vm, args, 2, "replace")?;
        new_str(vm, &s(args).replace(from, to))
    });
}
