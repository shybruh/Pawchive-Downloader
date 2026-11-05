import { useSyncExternalStore } from "react";

export type ViewId =
  | "downloader" | "queue" | "watchlist" | "activity"
  | "gallery" | "history" | "archive" | "known" | "vault"
  | "decompressor" | "scheduler" | "settings";

type NavState = { view: ViewId; params: Record<string, string> };

let state: NavState = { view: "downloader", params: {} };
const listeners = new Set<() => void>();

/** Switches screens. Params let one screen open another at a place (e.g. the Gallery at a folder). */
export function navigate(view: ViewId, params: Record<string, string> = {}) {
  state = { view, params };
  listeners.forEach((l) => l());
}

export function useNav(): NavState {
  return useSyncExternalStore((l) => { listeners.add(l); return () => { listeners.delete(l); }; }, () => state);
}
