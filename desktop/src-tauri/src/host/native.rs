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
        json!({"props": props, "methods": methods, "signals": self.signals()})
    }
}

/// The Rust-owned objects, by name.
#[derive(Clone, Default)]
pub struct Natives(Arc<HashMap<String, Arc<dyn NativeObject>>>);

impl Natives {
    pub fn new(objects: Vec<(&str, Arc<dyn NativeObject>)>) -> Self {
        Self(Arc::new(objects.into_iter().map(|(n, o)| (n.to_string(), o)).collect()))
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn NativeObject>> {
        self.0.get(name)
    }

    pub fn describe_all(&self) -> Map<String, Value> {
        self.0.iter().map(|(n, o)| (n.clone(), o.describe())).collect()
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
            "rows" => Err("not a list model".into()),
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
