//! embind / emval imports.
//!
//! The engine's C++ reaches into JS through `emscripten::val`. There is no JS here, so
//! values are modelled as property paths ("navigator.gpu", ...) and every call is logged
//! once. Calls the engine turns out to depend on get native answers in `call()`.

use std::collections::{HashMap, HashSet};

use wasmtime::{format_err as anyhow, Result};
use parking_lot::Mutex;
use wasmtime::*;

use crate::rt::Ctx;
use crate::{log, mem};

type Cx<'a> = Caller<'a, Ctx>;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    /// A host object identified by its path from globalThis ("" = globalThis).
    Obj(String),
    /// A plain JS object/array (from JSON.parse).
    Json(std::sync::Arc<serde_json::Value>),
}

fn from_json(v: &serde_json::Value) -> Val {
    use serde_json::Value as J;
    match v {
        J::Null => Val::Null,
        J::Bool(b) => Val::Bool(*b),
        J::Number(n) => Val::Num(n.as_f64().unwrap_or(0.0)),
        J::String(s) => Val::Str(s.clone()),
        other => Val::Json(std::sync::Arc::new(other.clone())),
    }
}

fn to_json(v: &Val) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Val::Undefined | Val::Null | Val::Obj(_) => J::Null,
        Val::Bool(b) => J::Bool(*b),
        Val::Num(n) => serde_json::Number::from_f64(*n).map_or(J::Null, J::Number),
        Val::Str(s) => J::String(s.clone()),
        Val::Json(j) => (**j).clone(),
    }
}

impl Val {
    fn describe(&self) -> String {
        match self {
            Val::Undefined => "undefined".into(),
            Val::Null => "null".into(),
            Val::Bool(b) => b.to_string(),
            Val::Num(n) => n.to_string(),
            Val::Str(s) => format!("{s:?}"),
            Val::Obj(p) => if p.is_empty() { "globalThis".into() } else { p.clone() },
            Val::Json(j) => {
                let t = j.to_string();
                if t.len() > 80 { format!("{}...", &t[..t.char_indices().nth(80).map_or(t.len(), |c| c.0)]) } else { t }
            }
        }
    }
}

/// Per-thread handle table (each worker had its own JS realm).
pub struct Handles {
    vals: HashMap<u32, (Val, u32)>,
    free: Vec<u32>,
    next: u32,
}

impl Handles {
    pub fn new() -> Self {
        Self { vals: HashMap::new(), free: Vec::new(), next: 10 }
    }
    fn to_handle(&mut self, v: Val) -> u32 {
        match v {
            Val::Undefined => 2,
            Val::Null => 4,
            Val::Bool(true) => 6,
            Val::Bool(false) => 8,
            v => {
                let h = self.free.pop().unwrap_or_else(|| {
                    let h = self.next;
                    self.next += 2;
                    h
                });
                self.vals.insert(h, (v, 1));
                h
            }
        }
    }
    fn to_value(&self, h: u32) -> Val {
        match h {
            2 => Val::Undefined,
            4 => Val::Null,
            6 => Val::Bool(true),
            8 => Val::Bool(false),
            h => self.vals.get(&h).map(|(v, _)| v.clone()).unwrap_or(Val::Undefined),
        }
    }
    fn replace(&mut self, h: u32, v: Val) {
        if let Some(e) = self.vals.get_mut(&h) {
            e.0 = v;
        }
    }
    fn incref(&mut self, h: u32) {
        if h > 9 {
            if let Some(e) = self.vals.get_mut(&h) {
                e.1 += 1;
            }
        }
    }
    fn decref(&mut self, h: u32) {
        if h > 9 {
            if let Some(e) = self.vals.get_mut(&h) {
                e.1 = e.1.saturating_sub(1);
                if e.1 == 0 {
                    self.vals.remove(&h);
                    self.free.push(h);
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Ty {
    Void,
    Bool,
    Int { size: u32, signed: bool },
    BigInt { signed: bool },
    Float { size: u32 },
    StdString,
    WString { char_size: u32 },
    Emval,
    MemoryView,
}

struct Invoker {
    ret: Ty,
    args: Vec<Ty>,
    kind: i32,
}

struct Registry {
    types: HashMap<u32, (String, Ty)>,
    invokers: Vec<Invoker>,
    logged: HashSet<String>,
}

static REG: Mutex<Option<Registry>> = Mutex::new(None);

fn with_reg<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut g = REG.lock();
    let r = g.get_or_insert_with(|| Registry { types: HashMap::new(), invokers: Vec::new(), logged: HashSet::new() });
    f(r)
}

fn reg_type(raw: i32, name: i32, ty: Ty) {
    let name = mem::cstr(name as u32);
    with_reg(|r| {
        r.types.insert(raw as u32, (name, ty));
    });
}

fn log_once(key: String) {
    let fresh = with_reg(|r| r.logged.insert(key.clone()));
    if fresh {
        log::info(&format!("emval: {key}"));
    }
}

/// readValueFromPointer for each wire type (8-byte generic slots).
fn read_arg(hs: &mut Handles, ty: &Ty, p: u32) -> Val {
    match ty {
        Ty::Void => Val::Undefined,
        Ty::Bool => Val::Bool(mem::u8(p) != 0),
        Ty::Int { size: 1, signed } => Val::Num(if *signed { mem::u8(p) as i8 as f64 } else { mem::u8(p) as f64 }),
        Ty::Int { size: 2, signed } => {
            let v = mem::u32(p) as u16;
            Val::Num(if *signed { v as i16 as f64 } else { v as f64 })
        }
        Ty::Int { signed, .. } => Val::Num(if *signed { mem::i32(p) as f64 } else { mem::u32(p) as f64 }),
        Ty::BigInt { signed } => Val::Num(if *signed { mem::u64(p) as i64 as f64 } else { mem::u64(p) as f64 }),
        Ty::Float { size: 4 } => Val::Num(mem::f32(p) as f64),
        Ty::Float { .. } => Val::Num(mem::f64(p)),
        Ty::StdString => {
            let s = mem::u32(p);
            let len = mem::u32(s) as usize;
            Val::Str(String::from_utf8_lossy(mem::bytes(s + 4, len)).into_owned())
        }
        Ty::WString { .. } => Val::Str("<wstring>".into()),
        Ty::Emval => {
            let h = mem::u32(p);
            let v = hs.to_value(h);
            hs.decref(h);
            v
        }
        Ty::MemoryView => Val::Str("<memory view>".into()),
    }
}

/// What the host answers for property reads. Unknown objects stay as paths.
fn get_property(obj: &Val, key: &Val) -> Val {
    let key = match key {
        Val::Str(s) => s.clone(),
        Val::Num(n) => n.to_string(),
        other => other.describe(),
    };
    if let Val::Json(j) = obj {
        use serde_json::Value as J;
        return match (&**j, key.as_str()) {
            (J::Array(a), "length") => Val::Num(a.len() as f64),
            (J::Array(a), k) => k.parse::<usize>().ok().and_then(|i| a.get(i)).map_or(Val::Undefined, from_json),
            (J::Object(o), k) => o.get(k).map_or(Val::Undefined, from_json),
            _ => Val::Undefined,
        };
    }
    if let Val::Str(s) = obj {
        return if key == "length" { Val::Num(s.encode_utf16().count() as f64) } else { Val::Undefined };
    }
    match obj {
        Val::Obj(path) => {
            let full = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
            match full.as_str() {
                // Feature probes the engine might make; answer like a capable browser.
                "crossOriginIsolated" => Val::Bool(true),
                "navigator.hardwareConcurrency" => {
                    Val::Num(std::thread::available_parallelism().map_or(8, |n| n.get()) as f64)
                }
                "navigator.gpu" => Val::Undefined,
                "devicePixelRatio" => Val::Num(1.0),
                _ => Val::Obj(full),
            }
        }
        _ => Val::Undefined,
    }
}

/// String(x) for a JSON value, as Array.prototype.join/sort see it.
fn js_string(v: &serde_json::Value) -> String {
    use serde_json::Value as J;
    match v {
        J::Null => String::new(),
        J::String(s) => s.clone(),
        J::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        J::Object(_) => "[object Object]".into(),
        other => other.to_string(),
    }
}

/// Calls the engine makes into JS. Logged once each; answer undefined unless known.
fn call(this: &Val, method: Option<&str>, target: &str, args: &[Val]) -> Val {
    let shown: Vec<String> = args.iter().map(|a| a.describe()).collect();
    let key = match this {
        Val::Json(_) | Val::Str(_) => format!("call <value>.{}()", method.unwrap_or("")),
        _ => format!("call {target}({})", shown.join(", ")),
    };
    log_once(key);
    let arg0 = args.first().cloned().unwrap_or(Val::Undefined);
    if let (Val::Json(j), Some(m)) = (this, method) {
        use serde_json::Value as J;
        let k = match &arg0 { Val::Str(s) => s.clone(), Val::Num(n) => n.to_string(), _ => String::new() };
        return match (m, &**j) {
            ("hasOwnProperty", J::Object(o)) => Val::Bool(o.contains_key(&k)),
            ("includes", J::Array(a)) => Val::Bool(a.contains(&to_json(&arg0))),
            ("indexOf", J::Array(a)) => Val::Num(a.iter().position(|x| *x == to_json(&arg0)).map_or(-1.0, |i| i as f64)),
            // Default sort: compare the elements' string forms (Array.prototype.sort).
            ("sort", J::Array(a)) => {
                let mut v = a.clone();
                v.sort_by(|x, y| js_string(x).cmp(&js_string(y)));
                Val::Json(std::sync::Arc::new(J::Array(v)))
            }
            ("reverse", J::Array(a)) => Val::Json(std::sync::Arc::new(J::Array(a.iter().rev().cloned().collect()))),
            ("join", J::Array(a)) => {
                let sep = match &arg0 { Val::Undefined => ",".to_string(), Val::Str(s) => s.clone(), v => v.describe() };
                Val::Str(a.iter().map(js_string).collect::<Vec<_>>().join(&sep))
            }
            ("slice", J::Array(a)) => {
                let len = a.len() as i64;
                let idx = |v: Option<&Val>, d: i64| match v {
                    Some(Val::Num(n)) => { let n = *n as i64; if n < 0 { (len + n).max(0) } else { n.min(len) } }
                    _ => d,
                };
                let (b, e) = (idx(args.first(), 0), idx(args.get(1), len));
                Val::Json(std::sync::Arc::new(J::Array(if b < e { a[b as usize..e as usize].to_vec() } else { vec![] })))
            }
            ("toString", J::Array(a)) => Val::Str(a.iter().map(js_string).collect::<Vec<_>>().join(",")),
            _ => Val::Undefined,
        };
    }
    match target {
        "JSON.parse" => match &arg0 {
            Val::Str(s) => match serde_json::from_str::<serde_json::Value>(s) {
                Ok(v) => from_json(&v),
                Err(e) => {
                    log::error(&format!("emval JSON.parse failed: {e}"));
                    Val::Undefined
                }
            },
            _ => Val::Undefined,
        },
        "JSON.stringify" => Val::Str(to_json(&arg0).to_string()),
        "Object.keys" => match &arg0 {
            Val::Json(j) => match &**j {
                serde_json::Value::Object(o) => Val::Json(std::sync::Arc::new(serde_json::Value::Array(
                    o.keys().map(|k| serde_json::Value::String(k.clone())).collect(),
                ))),
                serde_json::Value::Array(a) => Val::Json(std::sync::Arc::new(serde_json::Value::Array(
                    (0..a.len()).map(|i| serde_json::Value::String(i.to_string())).collect(),
                ))),
                _ => Val::Json(std::sync::Arc::new(serde_json::Value::Array(vec![]))),
            },
            _ => Val::Json(std::sync::Arc::new(serde_json::Value::Array(vec![]))),
        },
        "Object.values" | "Object.entries" => match &arg0 {
            Val::Json(j) => match &**j {
                serde_json::Value::Object(o) => Val::Json(std::sync::Arc::new(serde_json::Value::Array(if target == "Object.values" {
                    o.values().cloned().collect()
                } else {
                    o.iter().map(|(k, v)| serde_json::Value::Array(vec![serde_json::Value::String(k.clone()), v.clone()])).collect()
                }))),
                _ => Val::Json(std::sync::Arc::new(serde_json::Value::Array(vec![]))),
            },
            _ => Val::Json(std::sync::Arc::new(serde_json::Value::Array(vec![]))),
        },
        "Array.isArray" => Val::Bool(matches!(&arg0, Val::Json(j) if j.is_array())),
        "Number" => match &arg0 {
            Val::Num(n) => Val::Num(*n),
            Val::Str(s) => Val::Num(s.trim().parse().unwrap_or(f64::NAN)),
            Val::Bool(b) => Val::Num(*b as i32 as f64),
            _ => Val::Num(f64::NAN),
        },
        "String" => Val::Str(match &arg0 { Val::Str(s) => s.clone(), v => v.describe() }),
        "performance.now" => Val::Num(crate::log::uptime_ms()),
        "Date.now" => Val::Num(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_millis() as f64),
        ),
        _ => Val::Undefined,
    }
}

fn return_wire(cx: &mut Cx, ty: &Ty, v: Val, destructors_ref: u32) -> Result<f64> {
    // emval_returnValue: an (empty) destructors array is handed back as a handle.
    let dh = cx.data_mut().emval.to_handle(Val::Obj("<destructors>".into()));
    if destructors_ref != 0 {
        mem::set_u32(destructors_ref, dh);
    }
    Ok(match (ty, v) {
        (Ty::Void, _) => 0.0,
        (Ty::Emval, v) => cx.data_mut().emval.to_handle(v) as f64,
        (Ty::Bool, Val::Bool(b)) => b as i32 as f64,
        (Ty::Bool, _) => 0.0,
        (Ty::Int { .. } | Ty::Float { .. } | Ty::BigInt { .. }, Val::Num(n)) => n,
        (Ty::Int { .. } | Ty::Float { .. } | Ty::BigInt { .. }, Val::Bool(b)) => b as i32 as f64,
        (Ty::Int { .. } | Ty::Float { .. } | Ty::BigInt { .. }, Val::Str(s)) => s.trim().parse().unwrap_or(0.0),
        (Ty::Int { .. } | Ty::Float { .. } | Ty::BigInt { .. }, _) => 0.0,
        (Ty::StdString, v) => {
            // std::string wire: malloc'd [u32 length][bytes][NUL].
            let s = match v {
                Val::Str(s) => s,
                Val::Undefined => String::new(),
                Val::Json(j) => j.to_string(),
                other => other.describe(),
            };
            let ex = cx.data().ex().clone();
            let p = ex.malloc.call(&mut *cx, 4 + s.len() as i32 + 1)? as u32;
            mem::set_u32(p, s.len() as u32);
            mem::write_cstr(p + 4, &s, s.len() + 1);
            p as f64
        }
        (other, _) => return Err(anyhow!("emval: cannot return wire type {other:?}")),
    })
}

pub fn register(l: &mut Linker<Ctx>) -> Result<()> {
    l.func_wrap("env", "_embind_register_void", |_: Cx, raw: i32, name: i32| reg_type(raw, name, Ty::Void))?;
    l.func_wrap("env", "_embind_register_bool", |_: Cx, raw: i32, name: i32, _t: i32, _f: i32| {
        reg_type(raw, name, Ty::Bool)
    })?;
    l.func_wrap("env", "_embind_register_integer", |_: Cx, raw: i32, name: i32, size: i32, min: i32, _max: i32| {
        reg_type(raw, name, Ty::Int { size: size as u32, signed: min != 0 })
    })?;
    l.func_wrap("env", "_embind_register_bigint", |_: Cx, raw: i32, name: i32, _size: i32, min: i64, _max: i64| {
        reg_type(raw, name, Ty::BigInt { signed: min != 0 })
    })?;
    l.func_wrap("env", "_embind_register_float", |_: Cx, raw: i32, name: i32, size: i32| {
        reg_type(raw, name, Ty::Float { size: size as u32 })
    })?;
    l.func_wrap("env", "_embind_register_std_string", |_: Cx, raw: i32, name: i32| {
        reg_type(raw, name, Ty::StdString)
    })?;
    l.func_wrap("env", "_embind_register_std_wstring", |_: Cx, raw: i32, char_size: i32, name: i32| {
        reg_type(raw, name, Ty::WString { char_size: char_size as u32 })
    })?;
    l.func_wrap("env", "_embind_register_emval", |_: Cx, raw: i32| {
        with_reg(|r| {
            r.types.insert(raw as u32, ("emscripten::val".into(), Ty::Emval));
        })
    })?;
    l.func_wrap("env", "_embind_register_memory_view", |_: Cx, raw: i32, _idx: i32, name: i32| {
        reg_type(raw, name, Ty::MemoryView)
    })?;

    l.func_wrap("env", "_emval_incref", |mut cx: Cx, h: i32| cx.data_mut().emval.incref(h as u32))?;
    l.func_wrap("env", "_emval_decref", |mut cx: Cx, h: i32| cx.data_mut().emval.decref(h as u32))?;
    l.func_wrap("env", "_emval_run_destructors", |mut cx: Cx, h: i32| cx.data_mut().emval.decref(h as u32))?;
    l.func_wrap("env", "_emval_get_global", |mut cx: Cx, name: i32| -> i32 {
        let v = if name == 0 {
            Val::Obj(String::new())
        } else {
            get_property(&Val::Obj(String::new()), &Val::Str(mem::cstr(name as u32)))
        };
        cx.data_mut().emval.to_handle(v) as i32
    })?;
    l.func_wrap("env", "_emval_new_cstring", |mut cx: Cx, p: i32| -> i32 {
        cx.data_mut().emval.to_handle(Val::Str(mem::cstr(p as u32))) as i32
    })?;
    l.func_wrap("env", "_emval_get_property", |mut cx: Cx, h: i32, key: i32| -> i32 {
        let hs = &mut cx.data_mut().emval;
        let v = get_property(&hs.to_value(h as u32), &hs.to_value(key as u32));
        hs.to_handle(v) as i32
    })?;
    l.func_wrap("env", "_emval_typeof", |mut cx: Cx, h: i32| -> i32 {
        let hs = &mut cx.data_mut().emval;
        let t = match hs.to_value(h as u32) {
            Val::Undefined => "undefined",
            Val::Null | Val::Obj(_) | Val::Json(_) => "object",
            Val::Bool(_) => "boolean",
            Val::Num(_) => "number",
            Val::Str(_) => "string",
        };
        hs.to_handle(Val::Str(t.into())) as i32
    })?;
    l.func_wrap("env", "_emval_create_invoker", |_: Cx, argc: i32, types: i32, kind: i32| -> Result<i32> {
        with_reg(|r| {
            let mut tys = Vec::new();
            for i in 0..argc as u32 {
                let raw = mem::u32(types as u32 + 4 * i);
                let ty = r.types.get(&raw).map(|(_, t)| t.clone()).ok_or_else(|| anyhow!("emval: unregistered type {raw}"))?;
                tys.push(ty);
            }
            let ret = tys.remove(0);
            r.invokers.push(Invoker { ret, args: tys, kind });
            Ok((r.invokers.len() - 1) as i32)
        })
    })?;
    l.func_wrap(
        "env",
        "_emval_invoke",
        |mut cx: Cx, caller: i32, h: i32, method: i32, destructors_ref: i32, args: i32| -> Result<f64> {
            let (ret, arg_tys, kind) = with_reg(|r| {
                let inv = &r.invokers[caller as usize];
                (inv.ret.clone(), inv.args.clone(), inv.kind)
            });
            let (this, method_name, target, argv) = {
                let hs = &mut cx.data_mut().emval;
                let argv: Vec<Val> =
                    arg_tys.iter().enumerate().map(|(i, t)| read_arg(hs, t, args as u32 + 8 * i as u32)).collect();
                let this = hs.to_value(h as u32);
                let method_name = if kind == 1 { Some(mem::cstr(method as u32)) } else { None };
                let target = match kind {
                    1 => format!("{}.{}", this.describe(), mem::cstr(method as u32)),
                    2 => format!("new {}", this.describe()),
                    3 => "<bare>".to_string(),
                    _ => this.describe(),
                };
                (this, method_name, target, argv)
            };
            // kind 3 is a bare conversion (val::as<T>): the argument itself is returned.
            let v = if kind == 3 {
                argv.first().cloned().unwrap_or(Val::Undefined)
            } else {
                call(&this, method_name.as_deref(), &target, &argv)
            };
            // sort/reverse mutate the array in place in JS: the receiver's handle must see it.
            if matches!(method_name.as_deref(), Some("sort" | "reverse")) && matches!(v, Val::Json(_)) {
                cx.data_mut().emval.replace(h as u32, v.clone());
            }
            return_wire(&mut cx, &ret, v, destructors_ref as u32)
        },
    )?;
    Ok(())
}
