import { CircleIcon, GaugeIcon, PauseIcon, PlayIcon, SquareIcon, TimerIcon } from "lucide-react";
import { call, useProp } from "@/lib/engine";
import { navigate } from "@/lib/nav";
import { Button, Progress, Tip } from "@/components/ui";
import { percent } from "@/lib/utils";

/** Always-visible footer: what the engine is doing right now, with transport controls. */
export function StatusBar() {
  const onOpenQueue = () => navigate("queue");
  const downloading = useProp<boolean>("appBridge", "isDownloading", false);
  const paused = useProp<boolean>("appBridge", "isPaused", false);
  const status = useProp<string>("appBridge", "statusText", "Idle");
  const progress = useProp<number>("appBridge", "overallProgress", 0);
  const speed = useProp<string>("appBridge", "speedText", "");
  const eta = useProp<string>("appBridge", "etaText", "");
  const files = useProp<string>("appBridge", "filesCountText", "");
  const value = percent(progress);

  return (
    <footer className="flex h-11 shrink-0 items-center gap-4 border-t bg-sidebar px-4 text-xs">
      <button onClick={onOpenQueue} className="flex min-w-0 items-center gap-2 text-left">
        <CircleIcon className={downloading ? (paused ? "size-2 fill-warning text-warning" : "size-2 fill-success text-success") : "size-2 fill-muted-foreground/40 text-muted-foreground/40"} />
        <span className="truncate text-muted-foreground">{status || "Idle"}</span>
      </button>
      {downloading && (
        <>
          <Progress value={value} className="max-w-64 flex-1" />
          <span className="tabular-nums text-muted-foreground">{value.toFixed(0)}%</span>
          {files && <span className="hidden tabular-nums text-muted-foreground lg:inline">{files}</span>}
          {speed && <span className="flex items-center gap-1 tabular-nums text-muted-foreground"><GaugeIcon className="size-3.5" />{speed}</span>}
          {eta && <span className="hidden items-center gap-1 tabular-nums text-muted-foreground md:flex"><TimerIcon className="size-3.5" />{eta}</span>}
          <div className="ml-auto flex items-center gap-1">
            <Tip content={paused ? "Resume" : "Pause"}>
              <Button variant="ghost" size="icon-xs" onClick={() => call("appBridge", paused ? "resumeDownload" : "pauseDownload")}>
                {paused ? <PlayIcon /> : <PauseIcon />}
              </Button>
            </Tip>
            <Tip content="Stop">
              <Button variant="ghost" size="icon-xs" onClick={() => call("appBridge", "cancelDownload")}><SquareIcon /></Button>
            </Tip>
          </div>
        </>
      )}
    </footer>
  );
}
