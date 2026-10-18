//! What markup evaluates to: plain data describing elements, not elements. Nothing happens until a
//! template is mounted, so a template can be returned, stored, and mounted more than once.

use super::Vm;
use crate::heap::Header;
use crate::object::Kind;
use crate::value::{Obj, Value};

/// One piece of markup.
pub enum Tpl {
    /// `tag` is a string for an element, or a function for a component. `statics` holds the attributes
    /// that are constants, `dynamics` thunks for the ones that are not, and `children` is an array.
    Element { tag: Value, statics: Value, dynamics: Value, children: Value },
    Fragment { children: Value },
    /// `{expression}`: a thunk.
    Hole { func: Value },
    /// `<if>`: three thunks; the last is nil without an `<else>`.
    If { cond: Value, then: Value, els: Value },
    /// `<for>`: a thunk for the list, and a function of the item (and the key) that gives the children.
    For { iter: Value, body: Value },
}

#[repr(C)]
pub struct TemplateObj {
    pub header: Header,
    pub tpl: Tpl,
}

impl TemplateObj {
    pub(crate) fn trace(&self, f: &mut dyn FnMut(Value)) {
        match &self.tpl {
            Tpl::Element { tag, statics, dynamics, children } => {
                f(*tag);
                f(*statics);
                f(*dynamics);
                f(*children);
            }
            Tpl::Fragment { children } => f(*children),
            Tpl::Hole { func } => f(*func),
            Tpl::If { cond, then, els } => {
                f(*cond);
                f(*then);
                f(*els);
            }
            Tpl::For { iter, body } => {
                f(*iter);
                f(*body);
            }
        }
    }
}

/// What a component is called with: its attributes, read by name. An attribute written with an
/// expression is read through its thunk each time, so a computation that reads it follows the change.
#[repr(C)]
pub struct PropsObj {
    pub header: Header,
    pub statics: Value,
    pub dynamics: Value,
    pub children: Value,
}

impl Vm {
    pub(crate) fn new_template(&mut self, tpl: Tpl) -> Result<Value, Value> {
        match self.alloc(Kind::Template, |header| TemplateObj { header, tpl }) {
            Some(o) => Ok(Value::Obj(o)),
            None => Err(self.err("out of memory")),
        }
    }

    pub(crate) fn new_props(&mut self, statics: Value, dynamics: Value, children: Value) -> Result<Value, Value> {
        match self.alloc(Kind::Props, |header| PropsObj { header, statics, dynamics, children }) {
            Some(o) => Ok(Value::Obj(o)),
            None => Err(self.err("out of memory")),
        }
    }

    /// An attribute of a component, by name: `children`, one written with an expression (its thunk run
    /// now, so a computation that is running reads what the thunk reads), or a constant. Nil if not given.
    pub(crate) fn props_get(&mut self, props: Obj, sym: u32) -> Result<Value, Value> {
        let (statics, dynamics, children) = {
            let p = props.props();
            (p.statics, p.dynamics, p.children)
        };
        if self.syms.name(sym) == "children" {
            return Ok(children);
        }
        if let Value::Obj(d) = dynamics {
            if d.kind() == Kind::Object {
                if let Some(thunk) = d.object().fields.get(sym) {
                    return self.call(thunk, &[]);
                }
            }
        }
        if let Value::Obj(s) = statics {
            if s.kind() == Kind::Object {
                return Ok(s.object().fields.get(sym).unwrap_or(Value::Nil));
            }
        }
        Ok(Value::Nil)
    }
}
