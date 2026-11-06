import {
  ArrowLeftIcon, CircleCheckIcon, EraserIcon, CircleDashedIcon, CircleXIcon, EllipsisIcon, FileDownIcon, FileUpIcon, FilesIcon,
  FolderTreeIcon, ListOrderedIcon, Loader2Icon, PauseIcon, PlayIcon, RotateCwIcon, SkipForwardIcon, SquareIcon, TrashIcon,
} from "lucide-react";
import { toast } from "sonner";
import { call, useModel, useProp, useSetting } from "@/lib/engine";
import {
  Badge, Button, DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger,
  Empty, Progress, Segmented, Tip,
} from "@/components/ui";
import { PageHeader } from "@/components/page";
import { cn, pct } from "@/lib/utils";

type Task = {
  filename: string; postTitle: string; creatorName: string; service: string; status: string; progress: number;
  fileSize: string; downloadedBytes: string; errorMsg: string; speed: string; eta: string; fileId: string; retryCount: number;
  batchId: string;
};
type Group = {
  batchId: string; creatorName: string; postTitle: string; service: string; totalFiles: number; completedFiles: number;
  failedFiles: number; downloadingFiles: number; pendingFiles: number; skippedFiles: number; totalBytesStr: string;
  downloadedBytesStr: string; status: string; totalProgress: number; progress: number; activeFileName: string;
  activeFileSpeed: string; isPaused: boolean; pausedFiles: number;
};

type Tone = "success" | "info" | "warning" | "destructive" | "secondary" | "outline";
const STATUS: Record<string, { label: string; variant: Tone; icon: typeof CircleCheckIcon }> = {
  completed: { label: "Done", variant: "success", icon: CircleCheckIcon },
  downloading: { label: "Downloading", variant: "info", icon: Loader2Icon },
  retrying: { label: "Retrying", variant: "warning", icon: RotateCwIcon },
  pending: { label: "Waiting", variant: "outline", icon: CircleDashedIcon },
  paused: { label: "Paused", variant: "warning", icon: PauseIcon },
  failed: { label: "Failed", variant: "destructive", icon: CircleXIcon },
  cancelled: { label: "Cancelled", variant: "secondary", icon: SquareIcon },
  skipped: { label: "Skipped", variant: "secondary", icon: SkipForwardIcon },
  partial: { label: "Partly failed", variant: "warning", icon: CircleXIcon },
};

const Q = "appBridge.queueModel";

function StatusBadge({ status }: { status: string }) {
  const s = STATUS[status] ?? STATUS.pending;
  const Icon = s.icon;
  return <Badge variant={s.variant}><Icon className={cn(status === "downloading" && "animate-spin")} />{s.label}</Badge>;
}

export function QueueView() {
  const [filter, setFilter] = useSetting<string>(Q, "filterStatus", "all");
  const [mode, setMode] = useSetting<string>(Q, "viewMode", "flat");
  const [batch, setBatch] = useSetting<string>(Q, "selectedBatchId", "");
  const total = useProp<number>(Q, "totalCount", 0);
  const downloading = useProp<number>(Q, "downloadingCount", 0);
  const pending = useProp<number>(Q, "pendingCount", 0);
  const completed = useProp<number>(Q, "completedCount", 0);
  const failed = useProp<number>(Q, "failedCount", 0);
  const skipped = useProp<number>(Q, "skippedCount", 0);
  const isDownloading = useProp<boolean>("appBridge", "isDownloading", false);
  const paused = useProp<boolean>("appBridge", "isPaused", false);
  const grouped = mode === "grouped" && !batch;

  return (
    <>
      <PageHeader title="Queue" description={total ? `${total} files · ${completed} done · ${failed} failed` : "Nothing queued"}
        actions={<>
          {isDownloading && (
            <Button variant="outline" size="sm" onClick={() => call("appBridge", paused ? "resumeDownload" : "pauseDownload")}>
              {paused ? <PlayIcon /> : <PauseIcon />}{paused ? "Resume" : "Pause"}
            </Button>
          )}
          <Button variant="outline" size="sm" disabled={!failed} onClick={() => call("appBridge", "retryFailed")}><RotateCwIcon />Retry failed</Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm"><EllipsisIcon /></Button></DropdownMenuTrigger>
            <DropdownMenuContent>
              <DropdownMenuItem onSelect={() => call("appBridge", "exportQueueState")}><FileDownIcon />Export queue…</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => call("appBridge", "importQueueState")}><FileUpIcon />Import queue…</DropdownMenuItem>
              <DropdownMenuItem disabled={!failed} onSelect={() => call("appBridge", "exportFailedTasks")}><FileDownIcon />Export failed list…</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem disabled={!failed} onSelect={async () => {
                const n = await call<number>("appBridge", "deleteFailedLeftovers");
                toast.success(n ? `Deleted ${n} unfinished file${n === 1 ? "" : "s"}` : "Nothing to clean up");
              }}><EraserIcon />Delete failed files' leftovers</DropdownMenuItem>
              <DropdownMenuItem variant="destructive" disabled={!failed} onSelect={() => call("appBridge", "clearFailedTasks")}><TrashIcon />Clear failed</DropdownMenuItem>
              <DropdownMenuItem variant="destructive" disabled={!isDownloading} onSelect={() => call("appBridge", "cancelDownload")}><SquareIcon />Stop all</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </>}>
        {batch ? (
          <Button variant="ghost" size="sm" className="-ml-2" onClick={() => setBatch("")}><ArrowLeftIcon />All posts</Button>
        ) : (
          <Segmented value={filter} onChange={setFilter} options={[
            { value: "all", label: "All", count: total },
            { value: "downloading", label: "Active", count: downloading },
            { value: "pending", label: "Waiting", count: pending },
            { value: "completed", label: "Done", count: completed },
            { value: "failed", label: "Failed", count: failed },
            { value: "skipped", label: "Skipped", count: skipped },
          ]} />
        )}
        {!batch && (
          <Segmented className="ml-auto" value={mode} onChange={setMode} options={[
            { value: "grouped", label: <><FolderTreeIcon className="size-3.5" />By post</> },
            { value: "flat", label: <><FilesIcon className="size-3.5" />Files</> },
          ]} />
        )}
      </PageHeader>
      {grouped ? <GroupList onOpen={setBatch} /> : <FileList emptyAll={filter === "all" && !batch} />}
    </>
  );
}

function FileList({ emptyAll }: { emptyAll: boolean }) {
  const { rows, total } = useModel<Task>(Q, { limit: 800, throttleMs: 300 });
  if (rows.length === 0) {
    return (
      <Empty icon={ListOrderedIcon} title={emptyAll ? "The queue is empty" : "Nothing here"}
        description={emptyAll ? "Files appear here as soon as a download starts or you add links to the queue." : "No files match this filter."} />
    );
  }
  return (
    <div className="@container/list min-h-0 flex-1 overflow-y-auto">
      <div className="sticky top-0 z-10 flex items-center gap-4 border-b bg-background/95 px-6 py-2 text-xs font-medium text-muted-foreground backdrop-blur">
        <span className="min-w-0 flex-1">File</span>
        <span className="hidden w-40 @3xl/list:block">Creator</span>
        <span className="w-28">Status</span>
        <span className="w-40 @2xl/list:w-56">Progress</span>
        <span className="w-7" />
      </div>
      {rows.map((t, i) => {
        const p = pct(t.progress);
        return (
          <div key={`${t.fileId}-${i}`} className="group flex items-center gap-4 border-b border-border/60 px-6 py-2.5 hover:bg-accent/30">
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-medium" title={t.filename}>{t.filename}</div>
              <div className="truncate text-xs text-muted-foreground" title={t.errorMsg || t.postTitle}>
                {(t.status === "failed" || t.status === "cancelled") && t.errorMsg ? <span className="text-destructive">{t.errorMsg}</span> : t.postTitle}
              </div>
            </div>
            <div className="hidden w-40 truncate text-sm text-muted-foreground @3xl/list:block">{t.creatorName}</div>
            <div className="w-28"><StatusBadge status={t.status} /></div>
            <div className="flex w-40 flex-col gap-1 @2xl/list:w-56">
              <Progress value={p} indicatorClassName={t.status === "failed" ? "bg-destructive" : t.status === "completed" ? "bg-success" : undefined} />
              <span className="truncate text-[11px] text-muted-foreground tabular-nums">
                {t.status === "downloading"
                  ? [t.downloadedBytes && `${t.downloadedBytes} / ${t.fileSize}`, t.speed].filter(Boolean).join(" · ")
                  : t.fileSize || "—"}
              </span>
            </div>
            <div className="flex w-7 justify-end">
              {(t.status === "failed" || t.status === "cancelled") && (
                <Tip content="Retry this file">
                  <Button variant="ghost" size="icon-xs" onClick={() => call("appBridge", "retrySingleTask", t.fileId)}><RotateCwIcon /></Button>
                </Tip>
              )}
              {(t.status === "pending" || t.status === "downloading" || t.status === "retrying") && (
                <Tip content="Pause this file">
                  <Button variant="ghost" size="icon-xs" onClick={() => call(Q, "pauseTask", t.fileId)}><PauseIcon /></Button>
                </Tip>
              )}
              {t.status === "paused" && (
                <Tip content="Resume this file">
                  <Button variant="ghost" size="icon-xs" onClick={() => call(Q, "resumeTask", t.fileId)}><PlayIcon /></Button>
                </Tip>
              )}
            </div>
          </div>
        );
      })}
      {total > rows.length && <div className="px-6 py-3 text-xs text-muted-foreground">Showing the first {rows.length} of {total} files.</div>}
    </div>
  );
}

function GroupList({ onOpen }: { onOpen: (batchId: string) => void }) {
  const { rows } = useModel<Group>(`${Q}.groupsModel`, { throttleMs: 300 });
  if (rows.length === 0) {
    return <Empty icon={ListOrderedIcon} title="The queue is empty" description="Each post you download shows up here with its files." />;
  }
  const G = `${Q}`;
  return (
    <div className="@container/list min-h-0 flex-1 overflow-y-auto px-6 py-5">
      <div className="overflow-hidden rounded-xl border">
        {rows.map((g) => {
          const p = pct(g.totalProgress);
          const active = g.downloadingFiles > 0;
          const status = g.isPaused ? "paused" : active ? "downloading" : g.failedFiles > 0 && g.completedFiles + g.skippedFiles + g.failedFiles >= g.totalFiles
            ? (g.completedFiles > 0 ? "partial" : "failed") : g.completedFiles + g.skippedFiles >= g.totalFiles ? "completed" : "pending";
          return (
            <div key={g.batchId} className="group flex items-center gap-4 border-b px-4 py-3 last:border-b-0 hover:bg-accent/30">
              <button className="min-w-0 flex-1 text-left" onClick={() => onOpen(g.batchId)}>
                <div className="truncate text-sm font-medium">{g.postTitle || "Untitled post"}</div>
                <div className="truncate text-xs text-muted-foreground">
                  {g.creatorName}{g.service ? <span className="capitalize"> · {g.service}</span> : null}
                  {active && g.activeFileName ? <> · <span className="text-foreground/80">{g.activeFileName}</span></> : null}
                </div>
              </button>
              <div className="hidden w-28 @2xl/list:block"><StatusBadge status={status} /></div>
              <div className="flex w-40 flex-col gap-1 @3xl/list:w-56">
                <Progress value={p} indicatorClassName={status === "failed" ? "bg-destructive" : status === "completed" ? "bg-success" : undefined} />
                <span className="truncate text-[11px] text-muted-foreground tabular-nums">
                  {g.completedFiles}/{g.totalFiles} files{g.failedFiles ? ` · ${g.failedFiles} failed` : ""}{active && g.activeFileSpeed ? ` · ${g.activeFileSpeed}` : ""}
                </span>
              </div>
              <div className="flex w-16 justify-end gap-0.5">
                {(active || g.pendingFiles > 0 || g.isPaused) && (
                  <Tip content={g.isPaused ? "Resume this post" : "Pause this post"}>
                    <Button variant="ghost" size="icon-xs" onClick={() => call(G, g.isPaused ? "resumeBatch" : "pauseBatch", g.batchId)}>
                      {g.isPaused ? <PlayIcon /> : <PauseIcon />}
                    </Button>
                  </Tip>
                )}
                <DropdownMenu>
                  <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-xs"><EllipsisIcon /></Button></DropdownMenuTrigger>
                  <DropdownMenuContent>
                    <DropdownMenuItem onSelect={() => onOpen(g.batchId)}><FilesIcon />Show files</DropdownMenuItem>
                    <DropdownMenuItem disabled={!g.failedFiles} onSelect={() => call(G, "retryBatch", g.batchId)}><RotateCwIcon />Retry failed files</DropdownMenuItem>
                    <DropdownMenuItem onSelect={() => call(G, "cancelBatch", g.batchId)}><SquareIcon />Cancel remaining</DropdownMenuItem>
                    <DropdownMenuSeparator />
                    <DropdownMenuItem variant="destructive" onSelect={() => call(G, "removeBatch", g.batchId)}><TrashIcon />Remove from queue</DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
