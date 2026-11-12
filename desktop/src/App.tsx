import { useEffect, useSyncExternalStore } from "react";
import { Toaster, toast } from "sonner";
import { Loader2Icon, RotateCwIcon, TriangleAlertIcon } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { engine, resolveConnection, useEngineState, useSignal } from "@/lib/engine";
import { Button, TooltipProvider } from "@/components/ui";
import { AppSidebar } from "@/components/app-sidebar";
import { AppGuards } from "@/components/app-guards";
import { PostActionCountdown, RecoveryDialog } from "@/components/session-dialogs";
import { TelegramScope, TelegramSignIn } from "@/components/telegram";
import { FavoritesDialog } from "@/components/favorites";
import { CloudLinksDialog } from "@/components/cloud-links";
import { navigate, useNav } from "@/lib/nav";
import { checkForUpdate, loadVersion } from "@/lib/updater";
import { StatusBar } from "@/components/status-bar";
import { DownloaderView } from "@/views/downloader";
import { QueueView } from "@/views/queue";
import { WatchlistView } from "@/views/watchlist";
import { HistoryView } from "@/views/history";
import { SettingsView } from "@/views/settings";
import { ActivityView } from "@/views/activity";
import { GalleryView } from "@/views/gallery";
import { VaultView } from "@/views/vault";
import { DecompressorView } from "@/views/decompressor";
import { PostSelectionDialog } from "@/components/post-selection";
import logo from "@/assets/logo.png";

let stage = "Starting engine…";
const stageListeners = new Set<() => void>();
function setStage(s: string) {
  stage = s;
  if (import.meta.env.DEV) document.title = `Pawchive Downloader [${s}]`;
  stageListeners.forEach((l) => l());
}
function useStage() {
  return useSyncExternalStore((l) => { stageListeners.add(l); return () => { stageListeners.delete(l); }; }, () => stage);
}

// Single-flight: StrictMode runs effects twice in development, and a restart may overlap a mount.
let connecting: Promise<void> | null = null;
function connectEngine() {
  if (!connecting) {
    connecting = (async () => {
      try {
        setStage("Waiting for the engine…");
        const info = await resolveConnection();
        setStage(`Connecting on port ${info.port}…`);
        await engine.connect(info);
        if (import.meta.env.DEV) document.title = "Pawchive Downloader";
      } catch (e) {
        engine.fail(e instanceof Error ? e.message : String(e));
      } finally {
        connecting = null;
      }
    })();
  }
  return connecting;
}

async function restartEngine() {
  try {
    await invoke("engine_restart");
  } catch { /* browser dev mode: nothing to restart */ }
  await connectEngine();
}

export default function App() {
  const state = useEngineState();
  useEffect(() => { connectEngine(); }, []);
  const [status, error] = state.split("|");

  if (status !== "ready") return <Splash status={status} error={error} />;
  return (
    <TooltipProvider>
      <Shell />
      <Toaster theme="dark" position="bottom-right" richColors closeButton />
    </TooltipProvider>
  );
}

function Splash({ status, error }: { status: string; error: string }) {
  const failed = status === "error" || status === "closed";
  const currentStage = useStage();
  return (
    <div className="flex h-full flex-col items-center justify-center gap-6">
      <img src={logo} className="size-14 rounded-xl" alt="" />
      {failed ? (
        <div className="flex max-w-sm flex-col items-center gap-3 text-center">
          <div className="flex items-center gap-2 font-medium"><TriangleAlertIcon className="size-4 text-warning" />Engine unavailable</div>
          <p className="text-sm text-muted-foreground">{error || "The download engine stopped."}</p>
          <Button variant="outline" size="sm" onClick={restartEngine}><RotateCwIcon />Restart engine</Button>
        </div>
      ) : (
        <div className="flex items-center gap-2 text-sm text-muted-foreground"><Loader2Icon className="size-4 animate-spin" />{currentStage}</div>
      )}
    </div>
  );
}

function Shell() {
  const { view } = useNav();

  useEffect(() => {
    loadVersion();
    if (!import.meta.env.PROD) return;
    const t = setTimeout(async () => {
      if (await checkForUpdate()) {
        toast.info("A new version is available", { action: { label: "View", onClick: () => navigate("settings", { section: "updates" }) } });
      }
    }, 5000);
    return () => clearTimeout(t);
  }, []);

  // Engine-wide notifications
  useSignal("appBridge", "providerDisabled", (msg: string) => toast.warning(String(msg)));
  useSignal("appBridge", "exportCompleted", (msg: string) => toast.success(String(msg || "Export complete")));
  useSignal("appBridge", "exportFailed", (msg: string) => toast.error(String(msg || "Export failed")));
  useSignal("appBridge", "importCompleted", (msg: string) => toast.success(String(msg || "Import complete")));
  useSignal("appBridge", "importFailed", (msg: string) => toast.error(String(msg || "Import failed")));
  useSignal("appBridge", "postSelectionError", (msg: string) => toast.error(String(msg)));
  useSignal("appBridge", "watchlistCheckFinished", () => toast.success("Watchlist check finished"));
  // "Show in Gallery" from anywhere in the engine
  useSignal("appBridge", "galleryShowRequested", (path: string, isDir: boolean) =>
    navigate("gallery", { path: String(path || ""), isDir: isDir ? "1" : "0" }));

  const content = (() => {
    switch (view) {
      case "downloader": return <DownloaderView />;
      case "queue": return <QueueView />;
      case "watchlist": return <WatchlistView />;
      case "activity": return <ActivityView />;
      case "gallery": return <GalleryView />;
      case "history": return <HistoryView />;
      case "vault": return <VaultView />;
      case "decompressor": return <DecompressorView />;
      case "settings": return <SettingsView />;
    }
  })();

  return (
    <div className="flex h-full">
      <AppSidebar />
      <div className="flex min-w-0 flex-1 flex-col">
        <main className="@container/main flex min-h-0 flex-1 flex-col">{content}</main>
        <StatusBar />
      </div>
      <PostSelectionDialog />
      <RecoveryDialog />
      <TelegramSignIn />
      <TelegramScope />
      <FavoritesDialog />
      <CloudLinksDialog />
      <PostActionCountdown />
      <AppGuards />
    </div>
  );
}
