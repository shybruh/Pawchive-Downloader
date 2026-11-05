import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ArchiveIcon, CircleAlertIcon, CircleCheckIcon, DatabaseIcon, EllipsisIcon, EraserIcon, FileDownIcon, FileUpIcon,
  FolderSearchIcon, HardDriveIcon, ImageIcon, Loader2Icon, SearchIcon, ShieldCheckIcon, TrashIcon, UserRoundIcon,
} from "lucide-react";
import { toast } from "sonner";
import { call, callAsync, setProp, useModel, useProp, useSignal } from "@/lib/engine";
import { navigate } from "@/lib/nav";
import { pickFile, pickFolder, pickSavePath } from "@/lib/native";
import {
  Alert, Badge, Button, Dialog, DialogContent, DialogFooter, DropdownMenu, DropdownMenuContent, DropdownMenuItem,
  DropdownMenuSeparator, DropdownMenuTrigger, Empty, Input, Progress, Select, Tip,
} from "@/components/ui";
import { PageHeader } from "@/components/page";
import { cn } from "@/lib/utils";

type Stats = {
  total_files: number; total_creators: number; total_posts: number; db_size_str: string;
  category_counts: Record<string, number>; service_counts: Record<string, number>; verified_files: number; missing_files: number;
};
type Creator = {
  key: string; creator_name: string; creator_id: string; service: string; total_files: number; missing_count: number;
  verified_count: number; unverified_count: number; post_count: number; newest_downloaded_at?: string;
};
type FileRec = {
  id: number; filename: string; file_size_str: string; file_ext: string; downloaded_at_str: string; file_path: string; is_missing: number;
};
type Post = { post_id: string; post_title: string; service: string; missing_count: number; files: FileRec[] };

const TYPES = [
  { value: "all", label: "All types" }, { value: "images", label: "Images" }, { value: "videos", label: "Videos" },
  { value: "archives", label: "Archives" }, { value: "audio", label: "Audio" }, { value: "documents", label: "Documents" },
  { value: "graphics", label: "Graphics" }, { value: "links", label: "Links" }, { value: "other", label: "Other" },
];
const SORTS = [
  { value: "creator_az", label: "Name A–Z" }, { value: "creator_za", label: "Name Z–A" }, { value: "files_desc", label: "Most files" },
  { value: "newest", label: "Newest" }, { value: "oldest", label: "Oldest" },
];

function StatTile({ icon: Icon, label, value, hint }: { icon: typeof DatabaseIcon; label: string; value: string | number; hint?: string }) {
  return (
    <div className="flex min-w-0 items-center gap-3 rounded-xl border bg-card px-3 py-2.5 @4xl/main:px-4 @4xl/main:py-3">
      <div className="hidden size-9 shrink-0 items-center justify-center rounded-lg bg-muted @4xl/main:flex"><Icon className="size-4 text-muted-foreground" /></div>
      <div className="min-w-0">
        <div className="truncate text-xs text-muted-foreground">{label}</div>
        <div className="truncate text-base font-semibold tabular-nums">{value}{hint && <span className="ml-1.5 hidden text-xs font-normal text-muted-foreground @5xl/main:inline">{hint}</span>}</div>
      </div>
    </div>
  );
}

export function ArchiveView() {
  const enabled = useProp<boolean>("appBridge", "enableDownloadArchive", true);
  const rebuilding = useProp<boolean>("appBridge", "isArchiveRebuilding", false);
  const [query, setQuery] = useState("");
  const [service, setService] = useState("all");
  const [type, setType] = useState("all");
  const [sort, setSort] = useState("creator_az");
  const [stats, setStats] = useState<Stats | null>(null);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  const [rebuild, setRebuild] = useState<Record<string, number> | null>(null);
  const { rows, refresh } = useModel<{ creator: Creator }>("appBridge.archiveCreatorsModel", { throttleMs: 100 });
  const creators = useMemo(() => rows.map((r) => r.creator).filter(Boolean), [rows]);
  const [gen, setGen] = useState(0);

  const reload = useCallback(() => setGen((g) => g + 1), []);
  useEffect(() => {
    let cancelled = false;
    const t = setTimeout(async () => {
      setLoading(true);
      const res = await callAsync<{ count: number; statistics: Stats }>("appBridge", "getArchiveDataAsync", query, service, type, sort);
      if (cancelled) return;
      if (res?.statistics) setStats(res.statistics);
      await call("appBridge.archiveCreatorsModel", "applyPending");
      await refresh();
      setLoading(false);
    }, 250);
    return () => { cancelled = true; clearTimeout(t); };
  }, [query, service, type, sort, gen, refresh]);

  useSignal("appBridge", "archiveUpdated", reload);
  useSignal("appBridge", "archiveRebuildProgress", (d: Record<string, number>) => setRebuild(d));
  useSignal("appBridge", "archiveRebuildFinished", (d: Record<string, number>) => {
    setRebuild(null);
    toast.success(`Rebuild finished: ${d?.added_count ?? 0} files added`);
    reload();
  });

  const current = creators.find((c) => c.key === selected) ?? creators[0];
  const services = Object.keys(stats?.service_counts ?? {}).sort();

  const doExport = async () => {
    const path = await pickSavePath("Export the archive", "pawchive-archive.json", [
      { name: "Full archive (JSON)", extensions: ["json"] }, { name: "ID list (TXT)", extensions: ["txt"] },
    ]);
    if (!path) return;
    const n = await callAsync<number>("appBridge", "exportArchiveFileAsync", path, path.toLowerCase().endsWith(".txt") ? "txt" : "json");
    if (n >= 0) toast.success(`Exported ${n} records`);
    else toast.error(await call<string>("appBridge", "archiveLastError") || "Export failed");
  };
  const doImport = async () => {
    const path = await pickFile("Import an archive", [{ name: "Archive", extensions: ["json", "txt"] }]);
    if (!path) return;
    const r = await callAsync<unknown>("appBridge", "importArchiveFileAsync", path);
    if (r === false || r === -1) toast.error(await call<string>("appBridge", "archiveLastError") || "Import failed");
    else { toast.success("Archive imported"); reload(); }
  };
  const doRebuild = async () => {
    const dir = await pickFolder("Rebuild the archive from a folder");
    if (!dir) return;
    const ok = await call<boolean>("appBridge", "startArchiveRebuild", [dir], "full", true, true);
    if (!ok) toast.error("Couldn't start the rebuild");
  };

  return (
    <>
      <PageHeader title="Archive" description="Every file you've downloaded, so nothing is downloaded twice."
        actions={<>
          <Button variant="outline" size="sm" onClick={doImport}><FileUpIcon />Import</Button>
          <Button variant="outline" size="sm" onClick={doExport}><FileDownIcon />Export</Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm"><EllipsisIcon /></Button></DropdownMenuTrigger>
            <DropdownMenuContent className="w-60">
              <DropdownMenuItem disabled={rebuilding} onSelect={doRebuild}><FolderSearchIcon />Rebuild from a folder…</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onSelect={() => setConfirmClear(true)}><TrashIcon />Clear the archive…</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </>}>
        <div className="relative w-full max-w-72 min-w-44 flex-1">
          <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input className="h-8 pl-8" placeholder="Search creators, posts or files…" value={query} onChange={(e) => setQuery(e.target.value)} />
        </div>
        <Select size="sm" className="w-36" value={service} onValueChange={setService}
          options={[{ value: "all", label: "All services" }, ...services.map((s) => ({ value: s, label: s.charAt(0).toUpperCase() + s.slice(1) }))]} />
        <Select size="sm" className="w-32" value={type} onValueChange={setType} options={TYPES} />
        <Select size="sm" className="w-32" value={sort} onValueChange={setSort} options={SORTS} />
        {loading && <Loader2Icon className="size-4 animate-spin text-muted-foreground" />}
      </PageHeader>

      {!enabled && (
        <Alert className="mx-6 mt-4" tone="warning" icon={CircleAlertIcon}
          action={<Button size="xs" variant="outline" onClick={() => setProp("appBridge", "enableDownloadArchive", true)}>Turn on</Button>}>
          The archive is turned off, so new downloads aren't recorded.
        </Alert>
      )}
      {rebuild && (
        <Alert className="mx-6 mt-4" icon={Loader2Icon} busy
          action={<Button size="xs" variant="outline" onClick={() => call("appBridge", "cancelArchiveRebuild")}>Stop</Button>}>
          <div className="flex flex-col gap-1.5">
            <span>Rebuilding — {rebuild.processed_files ?? 0} of {rebuild.total_files ?? 0} files, {rebuild.added_count ?? 0} added</span>
            <Progress value={rebuild.percentage ?? 0} />
          </div>
        </Alert>
      )}

      {stats && (
        <div className="grid grid-cols-4 gap-3 px-6 pt-5">
          <StatTile icon={DatabaseIcon} label="Files" value={stats.total_files.toLocaleString()} hint={stats.db_size_str} />
          <StatTile icon={UserRoundIcon} label="Creators" value={stats.total_creators.toLocaleString()} hint={`${stats.total_posts.toLocaleString()} posts`} />
          <StatTile icon={CircleCheckIcon} label="Verified on disk" value={stats.verified_files.toLocaleString()} />
          <StatTile icon={CircleAlertIcon} label="Missing from disk" value={stats.missing_files.toLocaleString()} />
        </div>
      )}

      {creators.length === 0 ? (
        <Empty icon={ArchiveIcon} title={stats?.total_files ? "Nothing matches" : "The archive is empty"}
          description={stats?.total_files ? "Try another search or filter." : "Downloaded files are recorded here. You can also rebuild it from folders you already have."}>
          {!stats?.total_files && <Button size="sm" variant="outline" onClick={doRebuild}><FolderSearchIcon />Rebuild from a folder</Button>}
        </Empty>
      ) : (
        <div className="flex min-h-0 flex-1 px-6 py-5">
          <div className="flex min-h-0 flex-1 overflow-hidden rounded-xl border">
            <div className="w-64 shrink-0 overflow-y-auto border-r bg-sidebar/40 p-2">
              {creators.map((c) => (
                <button key={c.key} onClick={() => setSelected(c.key)}
                  className={cn("flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-left", current?.key === c.key ? "bg-accent" : "hover:bg-accent/50")}>
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">{c.creator_name}</div>
                    <div className="truncate text-xs text-muted-foreground"><span className="capitalize">{c.service}</span> · {c.post_count} posts</div>
                  </div>
                  <div className="flex flex-col items-end gap-0.5">
                    <span className="text-xs tabular-nums text-muted-foreground">{c.total_files}</span>
                    {c.missing_count > 0 && <span className="text-[10px] text-warning tabular-nums">{c.missing_count} missing</span>}
                  </div>
                </button>
              ))}
            </div>
            {current && <CreatorPosts key={current.key} creator={current} query={query} service={service} type={type} onChanged={reload} />}
          </div>
        </div>
      )}

      <Dialog open={confirmClear} onOpenChange={setConfirmClear}>
        <DialogContent title="Clear the archive?" description="Every record is removed and files you already have may be downloaded again. Your files on disk aren't touched.">
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmClear(false)}>Cancel</Button>
            <Button variant="destructive" onClick={async () => {
              setConfirmClear(false);
              await callAsync("appBridge", "clearDownloadArchiveAsync");
              toast.success("Archive cleared");
              reload();
            }}>Clear archive</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function CreatorPosts({ creator, query, service, type, onChanged }: { creator: Creator; query: string; service: string; type: string; onChanged: () => void }) {
  const [posts, setPosts] = useState<Post[] | null>(null);
  const [verifying, setVerifying] = useState<{ done: number; total: number } | null>(null);
  const [gen, setGen] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setPosts(null);
    callAsync<Post[]>("appBridge", "getArchiveCreatorPostsAsync", creator.key, query, service, type)
      .then((p) => { if (!cancelled) setPosts(p ?? []); });
    return () => { cancelled = true; };
  }, [creator.key, query, service, type, gen]);

  useSignal("appBridge", "archiveCreatorVerificationProgress", (_svc: string, cid: string, done: number, total: number) => {
    if (cid === creator.creator_id) setVerifying({ done, total });
  });
  useSignal("appBridge", "archiveCreatorVerificationFinished", (_svc: string, cid: string, result: Record<string, number>) => {
    if (cid !== creator.creator_id) return;
    setVerifying(null);
    toast.success(`Checked ${result?.total ?? creator.total_files} files: ${result?.missing ?? 0} missing`);
    setGen((g) => g + 1);
    onChanged();
  });

  const change = async (method: string, ...args: unknown[]) => {
    const ok = await callAsync<boolean>("appBridge", method, ...args);
    if (ok === false) toast.error(await call<string>("appBridge", "archiveLastError") || "That didn't work");
    setGen((g) => g + 1);
    onChanged();
  };

  return (
    <div className="flex min-w-0 flex-1 flex-col">
      <div className="flex items-center gap-3 border-b px-5 py-3">
        <div className="min-w-0 flex-1">
          <div className="truncate font-medium">{creator.creator_name}</div>
          <div className="truncate text-xs text-muted-foreground">
            {creator.total_files} files · {creator.verified_count} verified{creator.missing_count ? ` · ${creator.missing_count} missing` : ""}
          </div>
        </div>
        <Button variant="outline" size="sm" disabled={!!verifying}
          onClick={() => { setVerifying({ done: 0, total: creator.total_files }); call("appBridge", "verifyCreatorArchiveIntegrity", creator.service, creator.creator_id, creator.creator_name); }}>
          {verifying ? <Loader2Icon className="animate-spin" /> : <ShieldCheckIcon />}{verifying ? `${verifying.done}/${verifying.total}` : "Check files"}
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm"><EllipsisIcon /></Button></DropdownMenuTrigger>
          <DropdownMenuContent className="w-56">
            <DropdownMenuItem disabled={!creator.missing_count} onSelect={() => change("removeMissingArchiveRecordsForCreatorAsync", creator.service, creator.creator_id)}>
              <EraserIcon />Forget missing files
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onSelect={() => change("deleteArchiveCreatorAsync", creator.creator_id, creator.service)}><TrashIcon />Remove creator's records</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {posts === null ? (
          <div className="flex items-center gap-2 p-5 text-sm text-muted-foreground"><Loader2Icon className="size-4 animate-spin" />Loading posts…</div>
        ) : posts.length === 0 ? (
          <div className="p-5 text-sm text-muted-foreground">No posts match.</div>
        ) : posts.map((p) => (
          <section key={p.post_id} className="border-b last:border-b-0">
            <div className="group flex items-center gap-2 bg-muted/30 px-5 py-2">
              <span className="min-w-0 flex-1 truncate text-[13px] font-medium">{p.post_title || p.post_id}</span>
              <span className="text-xs text-muted-foreground tabular-nums">{p.files.length} files</span>
              <Tip content="Remove this post's records">
                <Button variant="ghost" size="icon-xs" className="opacity-0 group-hover:opacity-100" onClick={() => change("deleteArchivePostAsync", p.service, p.post_id)}><TrashIcon /></Button>
              </Tip>
            </div>
            {p.files.map((f) => (
              <div key={f.id} className="group flex items-center gap-3 px-5 py-1.5 hover:bg-accent/30">
                <Badge variant="outline" className="w-14 justify-center font-mono text-[10px]">{f.file_ext || "—"}</Badge>
                <span className={cn("min-w-0 flex-1 truncate text-[13px]", f.is_missing ? "text-muted-foreground line-through" : "")} title={f.file_path} data-selectable>{f.filename}</span>
                {f.is_missing ? <Badge variant="warning">Missing</Badge> : null}
                <span className="hidden w-20 text-right text-xs text-muted-foreground tabular-nums @3xl/main:block">{f.file_size_str}</span>
                <span className="hidden w-32 text-right text-xs text-muted-foreground tabular-nums @5xl/main:block">{f.downloaded_at_str}</span>
                <div className="flex w-20 justify-end opacity-0 group-hover:opacity-100">
                  {!f.is_missing && f.file_path && (
                    <Tip content="Show in Gallery">
                      <Button variant="ghost" size="icon-xs" onClick={() => navigate("gallery", { path: f.file_path, isDir: "0" })}><ImageIcon /></Button>
                    </Tip>
                  )}
                  {!f.is_missing && f.file_path && (
                    <Tip content="Show in Explorer">
                      <Button variant="ghost" size="icon-xs" onClick={() => call("appBridge", "revealFileInExplorer", f.file_path)}><HardDriveIcon /></Button>
                    </Tip>
                  )}
                  <Tip content="Forget this file">
                    <Button variant="ghost" size="icon-xs" onClick={() => change("deleteArchiveRecordAsync", f.id)}><TrashIcon /></Button>
                  </Tip>
                </div>
              </div>
            ))}
          </section>
        ))}
      </div>
    </div>
  );
}
