// App updates from GitHub Releases (Tauri updater plugin; releases are signed by the CI workflow).
import { useSyncExternalStore } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { inTauri } from "@/lib/native";

type Phase = "idle" | "checking" | "none" | "available" | "downloading" | "installing" | "error";
type State = { phase: Phase; version: string; latest: string; notes: string; date: string; progress: number; error: string };

let state: State = { phase: "idle", version: "", latest: "", notes: "", date: "", progress: 0, error: "" };
let pending: Update | null = null;
const listeners = new Set<() => void>();
const set = (patch: Partial<State>) => { state = { ...state, ...patch }; listeners.forEach((l) => l()); };

export function useUpdater() {
  return useSyncExternalStore((l) => { listeners.add(l); return () => { listeners.delete(l); }; }, () => state);
}

export async function loadVersion() {
  if (!inTauri()) { set({ version: "dev" }); return; }
  try { set({ version: await getVersion() }); } catch { /* not in the shell */ }
}

export async function checkForUpdate(): Promise<boolean> {
  if (!inTauri()) { set({ phase: "error", error: "Updates are only available in the installed app." }); return false; }
  set({ phase: "checking", error: "" });
  try {
    pending = await check();
    if (!pending) { set({ phase: "none" }); return false; }
    set({ phase: "available", latest: pending.version, notes: pending.body ?? "", date: pending.date ?? "" });
    return true;
  } catch (e) {
    set({ phase: "error", error: e instanceof Error ? e.message : String(e) });
    return false;
  }
}

/** Downloads the update, stops the engine so its files can be replaced, installs and restarts. */
export async function installUpdate() {
  if (!pending) return;
  let total = 0;
  let got = 0;
  try {
    set({ phase: "downloading", progress: 0 });
    await pending.download((e) => {
      if (e.event === "Started") total = e.data.contentLength ?? 0;
      else if (e.event === "Progress") { got += e.data.chunkLength; if (total) set({ progress: (got / total) * 100 }); }
    });
    set({ phase: "installing", progress: 100 });
    await invoke("engine_shutdown");
    await pending.install();          // on Windows the installer takes over and the app closes
    await relaunch();
  } catch (e) {
    set({ phase: "error", error: e instanceof Error ? e.message : String(e) });
  }
}
