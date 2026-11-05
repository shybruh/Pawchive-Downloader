import { useEffect, useMemo, useState } from "react";
import {
  ChevronRightIcon, CircleCheckIcon, CircleXIcon, EllipsisIcon, FileArchiveIcon, FolderOpenIcon, FolderPlusIcon, KeyRoundIcon,
  ListChecksIcon, Loader2Icon, LockIcon, PlayIcon, RefreshCwIcon, SearchIcon, Settings2Icon, SkipForwardIcon, SquareIcon,
  TrashIcon, TriangleAlertIcon, XIcon,
} from "lucide-react";
import { toast } from "sonner";
import { call, useModel, useProp, useSetting, useSignal } from "@/lib/engine";
import { pickFolder } from "@/lib/native";
import {
  Alert, Badge, Button, Dialog, DialogContent, DialogFooter, DropdownMenu, DropdownMenuContent, DropdownMenuItem,
  DropdownMenuSeparator, DropdownMenuTrigger, Empty, Input, Label, Progress, Switch, Textarea, Tip,
} from "@/components/ui";
import { Field, PageHeader } from "@/components/page";
import { cn, formatBytes } from "@/lib/utils";

const D = "decompressorBridge";

type GroupRow = { kind: "group"; rowKey: string; groupKey: string; creator: string; directory: string; totalCount: number; selectedCount: number; totalBytes: number; selectedBytes: number; collapsed: boolean };
type ItemRow = {
  kind: "item"; rowKey: string; id: string; filename: string; directory: string; scanRoot: string; size: number; creator: string;
  selected: boolean; status: string; progress: number; errorMessage: string; password: string; extractedPresent: boolean; extractedDir: string;
};
type Row = GroupRow | ItemRow;
type Location = { path: string; creator: string; source: "watchlist" | "custom" };
type Prompt = { itemId: string; filename: string; creator: string; error: string; size: number };

function Check({ state, onClick }: { state: "on" | "off" | "some"; onClick: () => void }) {
  return (
    <button onClick={onClick} className={cn("flex size-4 shrink-0 items-center justify-center rounded-[4px] border shadow-xs transition-colors",
      state === "off" ? "border-input bg-input/20" : "border-primary bg-primary text-primary-foreground")}>
      {state === "on" && <svg viewBox="0 0 16 16" className="size-3"><path d="M3.5 8.5l3 3 6-7" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" /></svg>}
      {state === "some" && <span className="h-0.5 w-2 rounded bg-current" />}
    </button>
  );
}

function ItemStatus({ it }: { it: ItemRow }) {
  switch (it.status) {
    case "done": return <Badge variant="success"><CircleCheckIcon />Extracted</Badge>;
    case "error": return <Badge variant="destructive"><CircleXIcon />Failed</Badge>;
    case "skipped": return <Badge variant="secondary"><SkipForwardIcon />Skipped</Badge>;
    case "password_required": return <Badge variant="warning"><LockIcon />Needs password</Badge>;
    case "extracting": return <Badge variant="info"><Loader2Icon className="animate-spin" />{Math.round(it.progress)}%</Badge>;
    default: return it.extractedPresent ? <Badge variant="outline">Already extracted</Badge> : <Badge variant="outline">Ready</Badge>;
  }
}

export function DecompressorView() {
  const scanning = useProp<boolean>(D, "isScanning", false);
  const extracting = useProp<boolean>(D, "isExtracting", false);
  const total = useProp<number>(D, "totalCount", 0);
  const selected = useProp<number>(D, "selectedCount", 0);
  const selectedBytes = useProp<number>(D, "selectedBytes", 0);
  const anyCollapsed = useProp<boolean>(D, "anyCollapsed", false);
  const has7za = useProp<boolean>(D, "has7za", true);
  const locationsJson = useProp<string>(D, "locationsJson", "[]");
  const locations = useMemo<Location[]>(() => { try { return JSON.parse(locationsJson || "[]"); } catch { return []; } }, [locationsJson]);
  const { rows } = useModel<{ creator: Row }>(`${D}.groupsModel`, { throttleMs: 150 });
  const list = useMemo(() => rows.map((r) => r.creator).filter(Boolean), [rows]);
  const [search, setSearch] = useState("");
  const [progress, setProgress] = useState<{ pct: number; name: string; eta: string; speed: string } | null>(null);
  const [optionsOpen, setOptionsOpen] = useState(false);
  const [bankOpen, setBankOpen] = useState(false);
  const [redo, setRedo] = useState<{ done: number; total: number } | null>(null);

  useEffect(() => { const t = setTimeout(() => call(D, "setSearch", search), 200); return () => clearTimeout(t); }, [search]);
  useSignal(D, "extractionStarted", () => setProgress({ pct: 0, name: "", eta: "", speed: "" }));
  useSignal(D, "extractionProgress", (pct: number, name: string, _cur: number, eta: string, speed: string) => setProgress({ pct, name, eta, speed }));
  useSignal(D, "extractionFinished", (ok: number, failed: number) => {
    setProgress(null);
    if (failed) toast.warning(`Extracted ${ok} archive${ok === 1 ? "" : "s"}, ${failed} failed`);
    else toast.success(`Extracted ${ok} archive${ok === 1 ? "" : "s"}`);
  });
  useSignal(D, "scanFinished", (count: number) => toast.success(`Found ${count} archive${count === 1 ? "" : "s"}`));
  useSignal(D, "scanError", (msg: string) => toast.error(String(msg)));
  useSignal(D, "redecompressConfirmRequested", (done: number, totalSel: number) => setRedo({ done, total: totalSel }));

  return (
    <>
      <PageHeader title="Decompressor" description={total ? `${total} archives · ${selected} selected (${formatBytes(selectedBytes)})` : "Find and extract downloaded archives in bulk."}
        actions={<>
          <Button variant="outline" size="sm" disabled={scanning || extracting} onClick={() => call(D, "scanLocations")}>
            {scanning ? <Loader2Icon className="animate-spin" /> : <RefreshCwIcon />}{scanning ? "Scanning…" : "Scan"}
          </Button>
          {extracting ? (
            <Button size="sm" variant="destructive" onClick={() => call(D, "cancelDecompression")}><SquareIcon />Stop</Button>
          ) : (
            <Button size="sm" disabled={!selected || !has7za} onClick={() => call(D, "startDecompression")}><PlayIcon />Extract {selected || ""}</Button>
          )}
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm"><EllipsisIcon /></Button></DropdownMenuTrigger>
            <DropdownMenuContent className="w-56">
              <DropdownMenuItem onSelect={async () => { const d = await pickFolder("Folder to scan for archives"); if (d) call(D, "addCustomFolder", d); }}><FolderPlusIcon />Add a folder…</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => setBankOpen(true)}><KeyRoundIcon />Passwords…</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => setOptionsOpen(true)}><Settings2Icon />Options…</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={() => call(D, "selectAll", true)}><ListChecksIcon />Select all</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => call(D, "selectAll", false)}><XIcon />Select none</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => call(D, "setAllCollapsed", !anyCollapsed)}><ChevronRightIcon />{anyCollapsed ? "Expand all" : "Collapse all"}</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={() => call(D, "clearFinished")}><TrashIcon />Clear finished</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </>}>
        <div className="relative w-full max-w-72 min-w-44 flex-1">
          <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input className="h-8 pl-8" placeholder="Filter archives…" value={search} onChange={(e) => setSearch(e.target.value)} />
        </div>
        <span className="text-xs text-muted-foreground">Scanning {locations.length} folder{locations.length === 1 ? "" : "s"}</span>
        {locations.filter((l) => l.source === "custom").map((l) => (
          <span key={l.path} className="inline-flex max-w-56 items-center gap-1 rounded-md border bg-card py-0.5 pr-0.5 pl-2 text-xs" title={l.path}>
            <span className="truncate">{l.creator}</span>
            <button className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => call(D, "removeCustomFolder", l.path)}><XIcon className="size-3" /></button>
          </span>
        ))}
      </PageHeader>

      {!has7za && (
        <Alert className="mx-6 mt-4" tone="warning" icon={TriangleAlertIcon}>7-Zip (7za) wasn't found next to the engine, so archives can't be extracted.</Alert>
      )}
      {progress && (
        <Alert className="mx-6 mt-4" icon={Loader2Icon} busy
          action={<span className="text-xs text-muted-foreground tabular-nums">{[progress.speed, progress.eta].filter(Boolean).join(" · ")}</span>}>
          <div className="flex flex-col gap-1.5">
            <span className="truncate">{progress.name || "Extracting…"}</span>
            <Progress value={progress.pct} />
          </div>
        </Alert>
      )}

      {list.length === 0 ? (
        <Empty icon={FileArchiveIcon} title={total ? "Nothing matches" : scanning ? "Scanning…" : "No archives found yet"}
          description={total ? "Try another filter." : "Scan your download folders (and any you add) for .zip, .rar and .7z files."}>
          {!total && !scanning && <Button size="sm" variant="outline" onClick={() => call(D, "scanLocations")}><RefreshCwIcon />Scan now</Button>}
        </Empty>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
          <div className="overflow-hidden rounded-xl border">
            {list.map((r) => r.kind === "group" ? <GroupHeader key={r.rowKey} g={r} /> : <Item key={r.rowKey} it={r} busy={extracting} />)}
          </div>
        </div>
      )}

      <OptionsDialog open={optionsOpen} onOpenChange={setOptionsOpen} />
      <PasswordBank open={bankOpen} onOpenChange={setBankOpen} />
      <PasswordPrompt />
      <Dialog open={!!redo} onOpenChange={(o) => !o && setRedo(null)}>
        <DialogContent title="Extract again?" description={redo ? `${redo.done} of the ${redo.total} selected archives were extracted before. Extract them again?` : ""}>
          <DialogFooter>
            <Button variant="outline" onClick={() => setRedo(null)}>Cancel</Button>
            <Button onClick={() => { setRedo(null); call(D, "confirmRedecompress"); }}>Extract again</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function GroupHeader({ g }: { g: GroupRow }) {
  const state = g.selectedCount === 0 ? "off" : g.selectedCount >= g.totalCount ? "on" : "some";
  return (
    <div className="sticky top-0 z-[1] flex items-center gap-3 border-b bg-muted/60 px-4 py-2 backdrop-blur">
      <button onClick={() => call(D, "toggleGroupCollapsed", g.groupKey)} className="rounded p-0.5 text-muted-foreground hover:text-foreground">
        <ChevronRightIcon className={cn("size-4 transition-transform", !g.collapsed && "rotate-90")} />
      </button>
      <Check state={state} onClick={() => call(D, "toggleCreatorByScanRoot", g.creator, g.directory)} />
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm font-medium">{g.creator}</div>
        <div className="truncate text-xs text-muted-foreground" title={g.directory}>{g.directory}</div>
      </div>
      <span className="text-xs text-muted-foreground tabular-nums">{g.selectedCount}/{g.totalCount} · {formatBytes(g.totalBytes)}</span>
    </div>
  );
}

function Item({ it, busy }: { it: ItemRow; busy: boolean }) {
  const [pw, setPw] = useState<string | null>(null);
  return (
    <div className="group flex items-center gap-3 border-b py-2 pr-4 pl-11 last:border-b-0 hover:bg-accent/30">
      <Check state={it.selected ? "on" : "off"} onClick={() => !busy && call(D, "toggleItem", it.id)} />
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm" title={it.filename}>{it.filename}</div>
        {it.status === "error" && it.errorMessage ? <div className="truncate text-xs text-destructive">{it.errorMessage}</div>
          : it.password ? <div className="flex items-center gap-1 truncate text-xs text-muted-foreground"><KeyRoundIcon className="size-3" />Password set</div> : null}
        {it.status === "extracting" && <Progress value={it.progress} className="mt-1.5 h-1" />}
      </div>
      <span className="w-20 text-right text-xs text-muted-foreground tabular-nums">{formatBytes(it.size)}</span>
      <div className="w-36"><ItemStatus it={it} /></div>
      <div className="flex w-14 justify-end gap-0.5">
        {(it.status === "done" || it.extractedPresent) && it.extractedDir && (
          <Tip content="Open extracted folder"><Button variant="ghost" size="icon-xs" onClick={() => call(D, "openFolder", it.extractedDir)}><FolderOpenIcon /></Button></Tip>
        )}
        {it.status !== "done" && (
          <Tip content="Set password"><Button variant="ghost" size="icon-xs" className="opacity-0 group-hover:opacity-100" onClick={() => setPw(it.password || "")}><KeyRoundIcon /></Button></Tip>
        )}
      </div>
      <Dialog open={pw !== null} onOpenChange={(o) => !o && setPw(null)}>
        <DialogContent title="Archive password" description={it.filename}>
          <Input autoFocus value={pw ?? ""} onChange={(e) => setPw(e.target.value)} placeholder="Password" className="font-mono" />
          <DialogFooter>
            <Button variant="outline" onClick={async () => {
              const ok = await call<boolean>(D, "testItemPassword", it.id, pw ?? "");
              if (ok) toast.success("That password works"); else toast.error("That password didn't work");
            }}>Test</Button>
            <Button onClick={() => {
              if (it.status === "error" || it.status === "password_required") call(D, "retryItemWithPassword", it.id, pw ?? "", true);
              else call(D, "setItemPassword", it.id, pw ?? "");
              setPw(null);
            }}>Save</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function PasswordPrompt() {
  const [queue, setQueue] = useState<Prompt[]>([]);
  const [pw, setPw] = useState("");
  const [remember, setRemember] = useState(true);
  const [wrong, setWrong] = useState(false);
  const current = queue[0];
  useSignal(D, "passwordPromptRequested", (itemId: string, filename: string, creator: string, _dir: string, error: string, size: number) => {
    setQueue((q) => (q.some((p) => p.itemId === itemId) ? q : [...q, { itemId, filename, creator, error, size }]));
  });
  useSignal(D, "passwordPromptDismissed", (itemId: string) => setQueue((q) => q.filter((p) => p.itemId !== itemId)));
  useSignal(D, "passwordWrong", (itemId: string) => { if (itemId === current?.itemId) setWrong(true); });
  useEffect(() => { setPw(""); setWrong(false); }, [current?.itemId]);
  if (!current) return null;
  const done = () => setQueue((q) => q.slice(1));
  return (
    <Dialog open onOpenChange={(o) => { if (!o) { call(D, "skipPasswordPrompt", current.itemId); done(); } }}>
      <DialogContent title="Password needed" description={`${current.filename} (${current.creator}) is locked.`}>
        <div className="flex flex-col gap-3">
          <Input autoFocus type="text" className={cn("font-mono", wrong && "border-destructive/60")} placeholder="Password" value={pw}
            onChange={(e) => { setPw(e.target.value); setWrong(false); }}
            onKeyDown={(e) => { if (e.key === "Enter" && pw) call(D, "submitPromptPassword", current.itemId, pw, remember); }} />
          {wrong && <p className="text-xs text-destructive">That password didn't work.</p>}
          <label className="flex items-center gap-2 text-sm"><Switch checked={remember} onCheckedChange={setRemember} />Remember it for this creator</label>
          {queue.length > 1 && <p className="text-xs text-muted-foreground">{queue.length - 1} more waiting.</p>}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => { call(D, "skipAllPasswordPrompts"); setQueue([]); }}>Skip all</Button>
          <Button variant="outline" onClick={() => { call(D, "skipPasswordPrompt", current.itemId); done(); }}>Skip</Button>
          <Button disabled={!pw} onClick={() => call(D, "submitPromptPassword", current.itemId, pw, remember)}>Unlock</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function OptionsDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const [parallel, setParallel] = useSetting<number>(D, "maxParallel", 2);
  const [threads, setThreads] = useSetting<number>(D, "threadsPerArchive", 2);
  const [deleteAfter, setDeleteAfter] = useSetting<boolean>(D, "deleteAfter", false);
  const [autoPrompt, setAutoPrompt] = useSetting<boolean>(D, "autoPromptPasswords", true);
  const [compress, setCompress] = useSetting<boolean>(D, "compressAfter", false);
  const ffmpeg = useProp<boolean>(D, "ffmpegAvailable", false);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent title="Decompressor options" className="max-w-xl">
        <div className="divide-y">
          <Field label="Archives at once"><Input type="number" min={1} max={8} className="w-20" value={parallel} onChange={(e) => setParallel(Math.max(1, Number(e.target.value) || 1))} /></Field>
          <Field label="Threads per archive"><Input type="number" min={1} max={16} className="w-20" value={threads} onChange={(e) => setThreads(Math.max(1, Number(e.target.value) || 1))} /></Field>
          <Field label="Ask for missing passwords" hint="Otherwise locked archives are skipped."><Switch checked={autoPrompt} onCheckedChange={setAutoPrompt} /></Field>
          <Field label="Move archives to the Recycle Bin afterwards" hint="Only after a successful extraction."><Switch checked={deleteAfter} onCheckedChange={setDeleteAfter} /></Field>
          <Field label="Compress extracted media" hint={ffmpeg ? "Shrinks pictures and videos after extracting." : "Needs ffmpeg."}>
            {!ffmpeg && <Button size="xs" variant="outline" onClick={() => call(D, "downloadFfmpeg")}>Get ffmpeg</Button>}
            <Switch checked={compress} disabled={!ffmpeg} onCheckedChange={setCompress} />
          </Field>
        </div>
        <DialogFooter><Button onClick={() => onOpenChange(false)}>Done</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function PasswordBank({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const passwords = useProp<string[]>(D, "savedPasswords", []);
  const [text, setText] = useState("");
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent title="Password bank" description="Tried on every locked archive, before asking you." className="max-w-xl">
        <div className="flex flex-col gap-3">
          <Label>Add passwords (one per line)</Label>
          <Textarea className="min-h-20 font-mono text-[13px]" value={text} onChange={(e) => setText(e.target.value)} />
          <div className="flex gap-2">
            <Button size="sm" disabled={!text.trim()} onClick={async () => { const n = await call<number>(D, "addMultiplePasswords", text); setText(""); toast.success(`Added ${n} password${n === 1 ? "" : "s"}`); }}>Add</Button>
            <Button size="sm" variant="outline" onClick={async () => { const n = await call<number>(D, "syncFromLinkVault"); toast.success(`Imported ${n} from the Link vault`); }}>Import from Link vault</Button>
          </div>
          <div className="max-h-56 overflow-y-auto rounded-lg border">
            {passwords.length === 0 ? <div className="p-3 text-sm text-muted-foreground">No saved passwords.</div> : passwords.map((p) => (
              <div key={p} className="group flex items-center gap-2 border-b px-3 py-1.5 last:border-b-0">
                <KeyRoundIcon className="size-3.5 text-muted-foreground" />
                <span className="min-w-0 flex-1 truncate font-mono text-[13px]" data-selectable>{p}</span>
                <Button variant="ghost" size="icon-xs" className="opacity-0 group-hover:opacity-100" onClick={() => call(D, "removePassword", p, "")}><XIcon /></Button>
              </div>
            ))}
          </div>
        </div>
        <DialogFooter>
          <Button variant="ghost" className="mr-auto text-destructive" disabled={!passwords.length} onClick={() => call(D, "clearSavedPasswords")}>Clear all</Button>
          <Button onClick={() => onOpenChange(false)}>Done</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
