//! Engine objects implemented in Rust.
//!
//! Each one answers the same requests the Python engine's objects do (`describe`, `get`, `set`,
//! `call`, `rows`) and pushes the same events (`props`, `signal`, `model`), so the UI can't tell
//! the difference. As parts of the engine are ported, their objects move here one at a time.

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Sends events (property changes, signals, model changes) to every connected UI.
#[derive(Clone)]
pub struct Events(broadcast::Sender<String>);

impl Events {
    pub fn new() -> Self {
        Self(broadcast::channel(1024).0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.0.subscribe()
    }

    #[allow(dead_code)]
    pub fn props(&self, obj: &str, values: Value) {
        let _ = self.0.send(json!({"event": "props", "obj": obj, "values": values}).to_string());
    }

    #[allow(dead_code)]
    pub fn signal(&self, obj: &str, name: &str, args: Vec<Value>) {
        let _ = self.0.send(json!({"event": "signal", "obj": obj, "name": name, "args": args}).to_string());
    }

    #[allow(dead_code)]
    pub fn model(&self, obj: &str, count: usize) {
        let _ = self.0.send(json!({"event": "model", "obj": obj, "count": count}).to_string());
    }
}

/// A property as `describe` reports it.
pub struct Prop {
    pub name: &'static str,
    pub kind: &'static str, // Qt-style type name the UI ignores but keeps ("bool", "QString"…)
    pub writable: bool,
}

pub trait NativeObject: Send + Sync {
    fn props(&self) -> Vec<Prop> {
        Vec::new()
    }
    /// method name -> parameter names
    fn methods(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        Vec::new()
    }
    fn signals(&self) -> Vec<&'static str> {
        Vec::new()
    }
    fn get(&self, prop: &str) -> Result<Value, String> {
        Err(format!("unknown property {prop:?}"))
    }
    fn set(&self, prop: &str, _value: Value) -> Result<Value, String> {
        Err(format!("{prop} is read-only"))
    }
    fn call(&self, method: &str, _args: Vec<Value>) -> Result<Value, String> {
        Err(format!("no method {method:?}"))
    }
    /// A list model's role names (None: not a list model)
    fn model_roles(&self) -> Option<&'static [&'static str]> {
        None
    }
    /// A list model's rows: {"total", "start", "rows": [{role: value}]}
    fn rows(&self, _start: usize, _count: Option<usize>) -> Result<Value, String> {
        Err("not a list model".into())
    }

    /// The object's entry in `describe`, in the Python engine's format.
    fn describe(&self) -> Value {
        let mut props = Map::new();
        for p in self.props() {
            let value = self.get(p.name).unwrap_or(Value::Null);
            props.insert(
                p.name.to_string(),
                json!({"type": p.kind, "writable": p.writable, "value": value, "lazy": false}),
            );
        }
        let methods: Map<String, Value> = self
            .methods()
            .into_iter()
            .map(|(m, params)| (m.to_string(), json!(params)))
            .collect();
        let mut out = json!({"props": props, "methods": methods, "signals": self.signals()});
        if let Some(roles) = self.model_roles() {
            // (numbered like Qt's: UserRole + 1, + 2…)
            let roles: Map<String, Value> = roles.iter().enumerate().map(|(i, r)| ((257 + i).to_string(), json!(r))).collect();
            let count = self.rows(0, Some(0)).ok().and_then(|r| r["total"].as_u64()).unwrap_or(0);
            out["model"] = json!({"count": count, "roles": roles});
        }
        out
    }
}

/// The Rust-owned objects, by name.
#[derive(Clone, Default)]
pub struct Natives(Arc<HashMap<String, Arc<dyn NativeObject>>>, Arc<HashMap<String, Arc<dyn Overlay>>>);

/// Some methods of an object Python still serves, answered in Rust instead (the object is
/// being ported a part at a time; Python keeps describing it and answering the rest)
pub trait Overlay: Send + Sync {
    fn handles(&self, method: &str) -> bool;
    fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String>;
    /// Properties answered here too: (name, Qt-style type); read-only, their changes sent as the
    /// object's "props" events by the Rust part
    fn props(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
    fn get(&self, _prop: &str) -> Option<Value> {
        None
    }
}

impl Natives {
    pub fn new(objects: Vec<(&str, Arc<dyn NativeObject>)>, overlays: Vec<(&str, Arc<dyn Overlay>)>) -> Self {
        Self(
            Arc::new(objects.into_iter().map(|(n, o)| (n.to_string(), o)).collect()),
            Arc::new(overlays.into_iter().map(|(n, o)| (n.to_string(), o)).collect()),
        )
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn NativeObject>> {
        self.0.get(name)
    }

    /// The Rust part of a Python object that answers this method
    pub fn overlay(&self, obj: &str, method: &str) -> Option<Arc<dyn Overlay>> {
        self.1.get(obj).filter(|o| o.handles(method)).cloned()
    }

    pub fn describe_all(&self) -> Map<String, Value> {
        self.0.iter().map(|(n, o)| (n.clone(), o.describe())).collect()
    }

    /// The Rust part of a Python object that answers this property
    pub fn overlay_prop(&self, obj: &str, prop: &str) -> Option<Arc<dyn Overlay>> {
        self.1.get(obj).filter(|o| o.props().iter().any(|(p, _)| *p == prop)).cloned()
    }

    /// Python's describe of the objects with Rust parts, with the properties answered in Rust
    pub fn patch_describe(&self, result: &mut Map<String, Value>) {
        for (name, ov) in self.1.iter() {
            let Some(props) = result.get_mut(name).and_then(|o| o.get_mut("props")).and_then(Value::as_object_mut) else { continue };
            for (p, kind) in ov.props() {
                props.insert(p.to_string(), json!({"type": kind, "writable": false, "value": ov.get(p).unwrap_or(Value::Null), "lazy": false}));
            }
        }
    }

    /// Python's "props" event for an object with Rust parts, without the properties Rust answers
    /// (None: nothing left of it)
    pub fn strip_props_event(&self, msg: &mut Value) -> Option<()> {
        let obj = msg.get("obj")?.as_str()?.to_string();
        let ov = self.1.get(&obj)?;
        let values = msg.get_mut("values")?.as_object_mut()?;
        for (p, _) in ov.props() {
            values.remove(p);
        }
        (!values.is_empty()).then_some(())
    }

    /// Answers one request for a native object.
    pub fn handle(&self, obj: &Arc<dyn NativeObject>, msg: &Value) -> Result<Value, String> {
        let s = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match msg.get("op").and_then(Value::as_str).unwrap_or("") {
            "get" => obj.get(&s("prop")),
            "set" => obj.set(&s("prop"), msg.get("value").cloned().unwrap_or(Value::Null)),
            "call" => {
                let args = msg.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
                obj.call(&s("method"), args)
            }
            "rows" => {
                let start = msg.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
                let count = msg.get("count").and_then(Value::as_u64).map(|c| c as usize);
                obj.rows(start, count)
            }
            op => Err(format!("unknown op {op:?}")),
        }
    }
}

/// "shell": facts about the app itself. Also proves the native path end to end.
pub struct Shell {
    pub version: String,
}

impl NativeObject for Shell {
    fn props(&self) -> Vec<Prop> {
        vec![
            Prop { name: "version", kind: "QString", writable: false },
            Prop { name: "engine", kind: "QString", writable: false },
        ]
    }
    fn methods(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        vec![("ping", vec![])]
    }
    fn get(&self, prop: &str) -> Result<Value, String> {
        match prop {
            "version" => Ok(json!(self.version)),
            // Which engine answers the rest (becomes "rust" once nothing is left in Python)
            "engine" => Ok(json!("rust+python")),
            _ => Err(format!("unknown property {prop:?}")),
        }
    }
    fn call(&self, method: &str, _args: Vec<Value>) -> Result<Value, String> {
        match method {
            "ping" => Ok(json!("pong")),
            _ => Err(format!("no method {method:?}")),
        }
    }
}
