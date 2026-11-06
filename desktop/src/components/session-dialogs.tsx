import { useEffect, useRef, useState } from "react";
import { HistoryIcon, PowerIcon, RotateCcwIcon } from "lucide-react";
import { call, useSignal } from "@/lib/engine";
import { Button, Dialog, DialogContent, DialogFooter, Progress } from "@/components/ui";

type Recovery = {
  total?: number; completed_files?: number; failed_files?: number; pending_files?: number; percent?: number;
  formatted_downloaded?: string; formatted_total?: string; saved_at?: string;
  artists?: { name?: string; service?: string; platform?: string }[];
};

/** After a crash or forced close, offers to pick the interrupted downloads back up. */
export function RecoveryDialog() {
  const [info, setInfo] = useState<Recovery | null>(null);
  useSignal("appBridge", "recoverySessionDetected", (s: Recovery) => setInfo(s && typeof s === "object" ? s : {}));
  useEffect(() => { call("appBridge", "checkRecoverySession").catch(() => {}); }, []);

  const names = (info?.artists ?? []).map((a) => a.name).filter(Boolean) as string[];
  const left = info ? (info.pending_files ?? 0) + (info.failed_files ?? 0) : 0;
  return (
    <Dialog open={!!info} onOpenChange={(o) => !o && setInfo(null)}>
      <DialogContent title="Pick up where you left off?" description="The app closed while downloads were still running.">
        {info && (
          <div className="flex flex-col gap-3 rounded-lg border bg-card p-4 text-sm">
            <div className="flex items-center gap-2 font-medium">
              <HistoryIcon className="size-4 text-muted-foreground" />
              {names.length ? names.slice(0, 3).join(", ") + (names.length > 3 ? ` and ${names.length - 3} more` : "") : "Previous session"}
            </div>
            <Progress value={info.percent ?? 0} />
            <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground tabular-nums">
              <span>{info.completed_files ?? 0} of {info.total ?? 0} files done</span>
              {left > 0 && <span>{left} left</span>}
              {info.formatted_total && <span>{info.formatted_downloaded ?? "0"} of {info.formatted_total}</span>}
              {info.saved_at && <span>Saved {String(info.saved_at).replace("T", " ").slice(0, 16)}</span>}
            </div>
          </div>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={() => { call("appBridge", "discardRecoverySession"); setInfo(null); }}>Start fresh</Button>
          <Button onClick={() => { call("appBridge", "resumeRecoverySession"); setInfo(null); }}><RotateCcwIcon />Resume downloads</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

const COUNTDOWN = 15;

/** "When the queue finishes": the engine asks for a short countdown before shutting down, sleeping… */
export function PostActionCountdown() {
  const [label, setLabel] = useState<string | null>(null);
  const [left, setLeft] = useState(COUNTDOWN);
  const done = useRef(false);
  useSignal("appBridge", "postActionCountdownStarted", (l: string) => { done.current = false; setLeft(COUNTDOWN); setLabel(String(l || "Action")); });

  useEffect(() => {
    if (label === null) return;
    if (left <= 0) {
      if (!done.current) { done.current = true; call("appBridge", "confirmPostAction"); setLabel(null); }
      return;
    }
    const t = setTimeout(() => setLeft((s) => s - 1), 1000);
    return () => clearTimeout(t);
  }, [label, left]);

  const cancel = () => { done.current = true; call("appBridge", "cancelPostAction"); setLabel(null); };
  return (
    <Dialog open={label !== null} onOpenChange={(o) => !o && cancel()}>
      <DialogContent title={`${label ?? ""} in ${left} s`} description="Downloads are finished.">
        <div className="flex items-center gap-3">
          <PowerIcon className="size-5 text-muted-foreground" />
          <Progress value={(left / COUNTDOWN) * 100} className="h-2" />
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={cancel}>Cancel</Button>
          <Button onClick={() => { done.current = true; call("appBridge", "confirmPostAction"); setLabel(null); }}>{label} now</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
