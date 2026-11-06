import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { call } from "@/lib/engine";

export const inTauri = () => "__TAURI_INTERNALS__" in window;

/** Folder picker: the shell's native dialog, or the engine's when running in a plain browser. */
export async function pickFolder(title = "Choose a folder", defaultPath?: string): Promise<string | null> {
  if (inTauri()) {
    const r = await open({ directory: true, title, defaultPath: defaultPath || undefined });
    return typeof r === "string" ? r : null;
  }
  const r = await call<string>("appBridge", "browseFolderDialog", title, defaultPath || "");
  return r || null;
}

/** File picker (one file). */
export async function pickFile(title = "Choose a file", filters?: { name: string; extensions: string[] }[]): Promise<string | null> {
  if (inTauri()) {
    const r = await open({ title, multiple: false, directory: false, filters });
    return typeof r === "string" ? r : null;
  }
  return window.prompt(title) || null;
}

/** Save-as picker. */
export async function pickSavePath(title: string, defaultPath: string, filters?: { name: string; extensions: string[] }[]): Promise<string | null> {
  if (inTauri()) return (await save({ title, defaultPath, filters })) ?? null;
  return window.prompt(title, defaultPath) || null;
}

export async function openExternal(url: string) {
  if (inTauri()) await openUrl(url);
  else window.open(url, "_blank");
}

export const clipboard = {
  async read(): Promise<string> {
    try {
      return inTauri() ? (await readText()) ?? "" : await navigator.clipboard.readText();
    } catch {
      return "";
    }
  },
  async write(text: string): Promise<void> {
    try {
      if (inTauri()) await writeText(text);
      else await navigator.clipboard.writeText(text);
    } catch {
      await call("appBridge", "copyToClipboard", text).catch(() => {});
    }
  },
};

export type SiteLoginResult = { site: string; cookie: string; names: string[] };

/**
 * Opens a site's login page in a window of the app and resolves with that window's cookies once
 * it closes (browsers lock their own cookies, so importing from them rarely works).
 */
export async function siteLogin(url: string): Promise<SiteLoginResult> {
  if (!inTauri()) throw new Error("Signing in here needs the desktop app");
  let stop: UnlistenFn | undefined;
  const done = new Promise<SiteLoginResult>((resolve) => {
    listen<SiteLoginResult>("site-login-finished", (e) => resolve(e.payload)).then((u) => (stop = u));
  });
  try {
    await invoke("site_login_open", { url });
    return await done;
  } finally {
    stop?.();
  }
}

export const siteLoginDone = () => invoke("site_login_done").catch(() => {});
export const siteLoginCancel = () => invoke("site_login_cancel").catch(() => {});
