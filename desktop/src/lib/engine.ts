// Client for the Python engine sidecar (server/sidecar.py).
//
// The engine's bridge objects are reflected generically: every Qt property, slot and signal is
// reachable by name. This module keeps a live mirror of all properties, so components can read
// them synchronously and re-render when they change.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";

type Json = any; // eslint-disable-line @typescript-eslint/no-explicit-any

export interface PropInfo { type: string; writable: boolean; value: Json }
export interface ObjectInfo {
  props: Record<string, PropInfo>;
  methods: Record<string, string[]>;
  signals: string[];
  model?: { count: number; roles: Record<string, string> };
}
export interface ConnectionInfo { port: number; token: string }
export type EngineState = "connecting" | "ready" | "error" | "closed";

type Pending = { resolve: (v: Json) => void; reject: (e: Error) => void };
type SignalHandler = (...args: Json[]) => void;

class EngineClient {
  state: EngineState = "connecting";
  error = "";
  objects: Record<string, ObjectInfo> = {};
  private ws: WebSocket | null = null;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private values = new Map<string, Json>();          // "obj.prop" → value
  private modelCounts = new Map<string, number>();
  private editSeq = new Map<string, number>();        // latest local edit per "obj.prop"
  private inflight = new Map<string, number>();       // local edits not yet answered
  private listeners = new Map<string, Set<() => void>>();
  private signalHandlers = new Map<string, Set<SignalHandler>>();
  private stateListeners = new Set<() => void>();
  private asyncSeq = 0;
  token = "";
  mediaPort = 0;
  /** The media server's secret (the Python engine's own when the UI talks through the host). */
  mediaToken = "";

  async connect(info: ConnectionInfo): Promise<void> {
    this.setState("connecting");
    const old = this.ws;
    this.ws = null;
    if (old) { old.onclose = null; old.close(); }
    await new Promise<void>((resolve, reject) => {
      const ws = new WebSocket(`ws://127.0.0.1:${info.port}/?token=${info.token}`);
      this.ws = ws;
      ws.onopen = () => resolve();
      ws.onerror = () => reject(new Error("Couldn't reach the engine"));
      ws.onclose = () => {
        if (this.ws !== ws) return;
        this.setState("closed", "The engine closed the connection");
        for (const p of this.pending.values()) p.reject(new Error("Engine disconnected"));
        this.pending.clear();
      };
      ws.onmessage = (e) => this.onMessage(JSON.parse(e.data));
    });
    const described: Record<string, ObjectInfo> = await this.request({ op: "describe" });
    this.objects = described;
    this.token = info.token;
    try {
      const meta = await this.request<{ mediaPort: number; mediaToken?: string }>({ op: "info" });
      this.mediaPort = meta.mediaPort || 0;
      this.mediaToken = meta.mediaToken || info.token;
    } catch { this.mediaPort = 0; }
    for (const [name, info] of Object.entries(described)) {
      for (const [p, pi] of Object.entries(info.props)) this.values.set(`${name}.${p}`, pi.value);
      if (info.model) this.modelCounts.set(name, info.model.count);
    }
    this.setState("ready");
  }

  fail(message: string) { this.setState("error", message); }

  private setState(s: EngineState, error = "") {
    this.state = s;
    this.error = error;
    this.stateListeners.forEach((l) => l());
  }

  private onMessage(msg: Json) {
    if (msg.id !== undefined) {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.ok) p.resolve(msg.result);
      else p.reject(new Error(msg.error));
      return;
    }
    if (msg.event === "props") {
      for (const [p, v] of Object.entries(msg.values)) {
        const key = `${msg.obj}.${p}`;
        if ((this.inflight.get(key) ?? 0) > 0) continue;   // our own edit is on its way
        this.values.set(key, v);
        this.notify(key);
      }
    } else if (msg.event === "model") {
      this.modelCounts.set(msg.obj, msg.count);
      this.notify(`model:${msg.obj}`);
    } else if (msg.event === "signal") {
      this.signalHandlers.get(`${msg.obj}.${msg.name}`)?.forEach((h) => h(...(msg.args ?? [])));
    }
  }

  private notify(key: string) { this.listeners.get(key)?.forEach((l) => l()); }

  private request<T = Json>(msg: Json): Promise<T> {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return Promise.reject(new Error("Engine not connected"));
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws!.send(JSON.stringify({ ...msg, id }));
    });
  }

  // ── Public API ──
  get<T = Json>(obj: string, prop: string): T { return this.values.get(`${obj}.${prop}`); }

  async set(obj: string, prop: string, value: Json) {
    const key = `${obj}.${prop}`;
    const seq = (this.editSeq.get(key) ?? 0) + 1;
    this.editSeq.set(key, seq);
    this.inflight.set(key, (this.inflight.get(key) ?? 0) + 1);
    this.values.set(key, value);       // optimistic: inputs stay responsive
    this.notify(key);
    try {
      const actual = await this.request({ op: "set", obj, prop, value });
      // Only the newest edit's answer counts; older answers would rewind what the user typed
      if (this.editSeq.get(key) === seq) {
        this.values.set(key, actual);
        this.notify(key);
      }
    } finally {
      this.inflight.set(key, (this.inflight.get(key) ?? 1) - 1);
    }
  }

  call<T = Json>(obj: string, method: string, ...args: Json[]): Promise<T> {
    return this.request<T>({ op: "call", obj, method, args });
  }

  /**
   * Engine methods named "…Async" take a request id first and answer later through the
   * asyncResultReady(requestId, result) signal. This turns that into a promise.
   */
  callAsync<T = Json>(obj: string, method: string, ...args: Json[]): Promise<T> {
    const id = `ui-${Date.now().toString(36)}-${++this.asyncSeq}`;
    return new Promise<T>((resolve, reject) => {
      const off = this.onSignal(obj, "asyncResultReady", (rid: string, result: Json) => {
        if (rid !== id) return;
        off();
        resolve(result as T);
      });
      this.request({ op: "call", obj, method, args: [id, ...args] }).catch((e) => { off(); reject(e); });
    });
  }

  /** Fetches a property now (for big values the UI only needs on demand). */
  fetch<T = Json>(obj: string, prop: string): Promise<T> {
    return this.request<T>({ op: "get", obj, prop }).then((v) => {
      this.values.set(`${obj}.${prop}`, v);
      this.notify(`${obj}.${prop}`);
      return v;
    });
  }

  mediaUrl(kind: "thumb" | "image" | "file", path: string): string {
    if (!this.mediaPort || !path) return "";
    return `http://127.0.0.1:${this.mediaPort}/${kind}?path=${encodeURIComponent(path)}&token=${this.mediaToken || this.token}`;
  }

  rows<T = Json>(obj: string, start = 0, count?: number): Promise<{ total: number; start: number; rows: T[] }> {
    return this.request({ op: "rows", obj, start, count });
  }

  modelCount(obj: string) { return this.modelCounts.get(obj) ?? 0; }

  subscribe(key: string, l: () => void) {
    let set = this.listeners.get(key);
    if (!set) this.listeners.set(key, (set = new Set()));
    set.add(l);
    return () => { set!.delete(l); };
  }

  onSignal(obj: string, name: string, h: SignalHandler) {
    const key = `${obj}.${name}`;
    let set = this.signalHandlers.get(key);
    if (!set) this.signalHandlers.set(key, (set = new Set()));
    set.add(h);
    return () => { set!.delete(h); };
  }

  onState(l: () => void) {
    this.stateListeners.add(l);
    return () => { this.stateListeners.delete(l); };
  }
}

export const engine = new EngineClient();

/** Where to find the engine: from the Tauri shell, or from Vite env vars when run in a browser. */
export async function resolveConnection(): Promise<ConnectionInfo> {
  const env = import.meta.env;
  if (env.VITE_ENGINE_PORT && env.VITE_ENGINE_TOKEN) {
    return { port: Number(env.VITE_ENGINE_PORT), token: String(env.VITE_ENGINE_TOKEN) };
  }
  return invoke<ConnectionInfo>("engine_info");
}

// ── Hooks ──────────────────────────────────────────────────────────────────────────────────────

export function useEngineState() {
  return useSyncExternalStore(
    (l) => engine.onState(l),
    () => `${engine.state}|${engine.error}`,
  );
}

/** Live value of an engine property. */
export function useProp<T = Json>(obj: string, prop: string, fallback?: T): T {
  const v = useSyncExternalStore(
    useCallback((l) => engine.subscribe(`${obj}.${prop}`, l), [obj, prop]),
    () => engine.get<T>(obj, prop),
  );
  return (v === undefined || v === null ? fallback : v) as T;
}

/** Live value plus a setter that writes it back to the engine. */
export function useSetting<T = Json>(obj: string, prop: string, fallback?: T): [T, (v: T) => void] {
  const value = useProp<T>(obj, prop, fallback);
  const set = useCallback((v: T) => { engine.set(obj, prop, v).catch(console.error); }, [obj, prop]);
  return [value, set];
}

/** Rows of a list model, re-fetched (throttled) whenever the model changes. */
export function useModel<T = Json>(obj: string, opts: { limit?: number; throttleMs?: number } = {}) {
  const { limit, throttleMs = 250 } = opts;
  const [rows, setRows] = useState<T[]>([]);
  const [total, setTotal] = useState(0);
  const timer = useRef<number | null>(null);
  const inflight = useRef(false);
  const again = useRef(false);

  const fetchRows = useCallback(async () => {
    if (inflight.current) { again.current = true; return; }
    inflight.current = true;
    try {
      const r = await engine.rows<T>(obj, 0, limit);
      setRows(r.rows);
      setTotal(r.total);
    } catch { /* engine restarting */ }
    inflight.current = false;
    if (again.current) { again.current = false; fetchRows(); }
  }, [obj, limit]);

  useEffect(() => {
    fetchRows();
    return engine.subscribe(`model:${obj}`, () => {
      if (timer.current !== null) return;
      timer.current = window.setTimeout(() => { timer.current = null; fetchRows(); }, throttleMs);
    });
  }, [obj, fetchRows, throttleMs]);

  return { rows, total, refresh: fetchRows };
}

export function useSignal(obj: string, name: string, handler: SignalHandler) {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => engine.onSignal(obj, name, (...a) => ref.current(...a)), [obj, name]);
}

/** Calls a method once on mount (and when deps change) and returns its result. */
export function useQuery<T = Json>(obj: string, method: string, args: Json[] = [], deps: unknown[] = []) {
  const [data, setData] = useState<T | undefined>(undefined);
  const [loading, setLoading] = useState(true);
  const load = useCallback(async () => {
    setLoading(true);
    try { setData(await engine.call<T>(obj, method, ...args)); } finally { setLoading(false); }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [obj, method, JSON.stringify(args)]);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => { load().catch(console.error); }, [load, ...deps]);
  return { data, loading, reload: load };
}

export const call = engine.call.bind(engine);
export const callAsync = engine.callAsync.bind(engine);
export const mediaUrl = engine.mediaUrl.bind(engine);
export const setProp = engine.set.bind(engine);
