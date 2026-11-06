import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowUpIcon, BookmarkIcon, BookmarkPlusIcon, ChevronLeftIcon, ChevronRightIcon, CopyIcon, DownloadIcon, EllipsisIcon,
  ExternalLinkIcon, FileIcon, FilmIcon, FolderIcon, FolderInputIcon, FolderOpenIcon, HardDriveIcon, ImageIcon, LayoutGridIcon,
  ListIcon, Loader2Icon, MusicIcon, PencilIcon, PlayIcon, RefreshCwIcon, SearchIcon, TrashIcon, XIcon,
} from "lucide-react";
import { toast } from "sonner";
import { call, mediaUrl, useProp, useSignal } from "@/lib/engine";
import { useNav } from "@/lib/nav";
import { MediaPlayer } from "@/components/media-player";
import { useElementWidth } from "@/lib/hooks";
import { clipboard, pickFolder } from "@/lib/native";
import {
  Button, Dialog, DialogContent, DialogFooter, DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel,
  DropdownMenuSeparator, DropdownMenuTrigger, Empty, Input, Segmented, Select, Tip,
} from "@/components/ui";
import { cn, formatBytes } from "@/lib/utils";

type Entry = { name: string; path: string; is_dir: boolean; size: number; file_count: number; folder_count: number; mtime: number; ext: string };
type Place = { name: string; path: string; free_str?: string };
type FolderStats = { size: number; files: number; dirs: number };

function folderSummary(e: Entry, st?: FolderStats) {
  const files = st ? st.files : e.file_count;
  const dirs = st ? st.dirs : e.folder_count;
  if (files < 0) return "";
  const parts = [`${files.toLocaleString()} file${files === 1 ? "" : "s"}`];
  if (dirs > 0) parts.push(`${dirs.toLocaleString()} folder${dirs === 1 ? "" : "s"}`);
  if (st && st.size > 0) parts.push(formatBytes(st.size));
  return parts.join(" · ");
}

const IMAGE = new Set(["jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "jfif", "heic", "heif", "tif", "tiff", "ico", "svg", "psd"]);
const VIDEO = new Set(["mp4", "m4v", "webm", "mov", "mkv", "avi", "wmv", "flv", "mpg", "mpeg", "ts", "3gp"]);
const AUDIO = new Set(["mp3", "flac", "wav", "ogg", "opus", "m4a", "aac", "wma"]);
const extOf = (e: Entry) => (e.ext || e.name.split(".").pop() || "").replace(".", "").toLowerCase();
const kindOf = (e: Entry): "dir" | "image" | "video" | "audio" | "file" =>
  e.is_dir ? "dir" : IMAGE.has(extOf(e)) ? "image" : VIDEO.has(extOf(e)) ? "video" : AUDIO.has(extOf(e)) ? "audio" : "file";

function parentOf(p: string): string {
  const trimmed = p.replace(/[\\/]+$/, "");
  const idx = Math.max(trimmed.lastIndexOf("\\"), trimmed.lastIndexOf("/"));
  if (idx <= 0) return p;
  const parent = trimmed.slice(0, idx);
  return /^[A-Za-z]:$/.test(parent) ? parent + "\\" : parent;
}

const SIZES = { s: 132, m: 184, l: 260 } as const;

export function GalleryView() {
  const nav = useNav();
  const [rootRef, rootWidth] = useElementWidth<HTMLDivElement>();
  const showPlaces = rootWidth >= 880;      // narrow windows fold the places into a menu
  const downloadDir = useProp<string>("appBridge", "downloadDir", "");
  const bookmarks = useProp<Place[]>("appBridge", "galleryBookmarks", []);
  const [drives, setDrives] = useState<Place[]>([]);
  const [path, setPath] = useState<string>("");
  const [entries, setEntries] = useState<Entry[]>([]);
  const [crumbs, setCrumbs] = useState<Place[]>([]);
  const [totalInFolder, setTotalInFolder] = useState(0);
  const [loading, setLoading] = useState(false);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<"all" | "image" | "video" | "dir">("all");
  const [sort, setSort] = useState("name");
  const [view, setView] = useState<"grid" | "list">(() => { try { return (localStorage.getItem("gallery.view") as "grid" | "list") || "grid"; } catch { return "grid"; } });
  const [size, setSize] = useState<keyof typeof SIZES>("m");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const anchor = useRef<number>(-1);
  const [viewer, setViewer] = useState<number | null>(null);
  const [renaming, setRenaming] = useState<Entry | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string[] | null>(null);
  const highlight = useRef<string>("");
  const back = useRef<string[]>([]);

  const load = useCallback(async (p: string, opts: { push?: boolean } = {}) => {
    if (!p) return;
    setLoading(true);
    try {
      const [list, bc, total] = await Promise.all([
        call<Entry[]>("appBridge", "listDirectory", p, 3000),
        call<Place[]>("appBridge", "getBreadcrumbs", p),
        call<number>("appBridge", "getLastListingTotal"),
      ]);
      if (opts.push !== false && path && path !== p) back.current.push(path);
      setPath(p);
      setEntries(list ?? []);
      setCrumbs(bc ?? []);
      setTotalInFolder(total ?? 0);
      setSelected(highlight.current ? new Set([highlight.current]) : new Set());
      anchor.current = -1;
    } finally {
      setLoading(false);
    }
  }, [path]);

  // Where to start: a place another screen asked for, else the download folder
  useEffect(() => {
    const target = nav.params.path;
    if (target) {
      if (nav.params.isDir === "0") { highlight.current = target; load(parentOf(target)); }
      else { highlight.current = ""; load(target); }
    } else if (!path) {
      call<string>("appBridge", "getDownloadDir").then((d) => load(d || downloadDir));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nav.params.path]);

  useEffect(() => { call<Place[]>("appBridge", "getSystemDrives").then((d) => setDrives(d ?? [])); }, []);
  useSignal("appBridge", "drivesUpdated", (d: Place[]) => setDrives(d ?? []));
  useEffect(() => { try { localStorage.setItem("gallery.view", view); } catch { /* storage off */ } }, [view]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    const list = entries.filter((e) => (!q || e.name.toLowerCase().includes(q)) && (kind === "all" || kindOf(e) === kind || (kind !== "dir" && e.is_dir)));
    const by = {
      name: (a: Entry, b: Entry) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }),
      newest: (a: Entry, b: Entry) => b.mtime - a.mtime,
      oldest: (a: Entry, b: Entry) => a.mtime - b.mtime,
      largest: (a: Entry, b: Entry) => b.size - a.size,
    }[sort] ?? (() => 0);
    return list.sort((a, b) => (a.is_dir === b.is_dir ? by(a, b) : a.is_dir ? -1 : 1));
  }, [entries, query, kind, sort]);
  const media = useMemo(() => visible.filter((e) => kindOf(e) === "image" || kindOf(e) === "video"), [visible]);
  const folderCount = useMemo(() => visible.filter((e) => e.is_dir).length, [visible]);
  const [stats, setStats] = useState<Record<string, FolderStats>>({});
  useSignal("appBridge", "folderStatsCalculated", (p: string, size: number, files: number, dirs: number) =>
    setStats((m) => ({ ...m, [String(p).toLowerCase()]: { size, files, dirs } })));

  // Scroll a highlighted file into view once it's listed
  useEffect(() => {
    if (!highlight.current) return;
    const el = document.querySelector(`[data-path="${CSS.escape(highlight.current)}"]`);
    if (el) { el.scrollIntoView({ block: "center" }); highlight.current = ""; }
  }, [visible]);

  const open = (e: Entry) => {
    const k = kindOf(e);
    if (k === "dir") load(e.path);
    else if (k === "image" || k === "video") setViewer(media.findIndex((m) => m.path === e.path));
    else call("appBridge", "openPathInSystem", e.path);
  };

  const click = (e: Entry, i: number, ev: React.MouseEvent) => {
    setSelected((prev) => {
      const next = new Set(ev.ctrlKey || ev.metaKey ? prev : []);
      if (ev.shiftKey && anchor.current >= 0) {
        const [a, b] = [Math.min(anchor.current, i), Math.max(anchor.current, i)];
        for (let k = a; k <= b; k++) next.add(visible[k].path);
      } else if ((ev.ctrlKey || ev.metaKey) && prev.has(e.path)) {
        next.delete(e.path);
      } else {
        next.add(e.path);
        anchor.current = i;
      }
      return next;
    });
  };

  const sel = useMemo(() => visible.filter((e) => selected.has(e.path)), [visible, selected]);

  const moveSelected = async () => {
    const dest = await pickFolder("Move to…", path);
    if (!dest) return;
    const r = await call<{ moved: number; failed: number; errors: string[] }>("appBridge", "moveItems", sel.map((e) => e.path), dest);
    if (r.failed) toast.error(r.errors?.[0] || `${r.failed} couldn't be moved`); else toast.success(`Moved ${r.moved} item${r.moved === 1 ? "" : "s"}`);
    load(path, { push: false });
  };
  const deletePaths = async (paths: string[]) => {
    const r = await call<{ deleted: number; failed: number; errors: string[] }>("appBridge", "deleteItems", paths);
    if (r.failed) toast.error(r.errors?.[0] || `${r.failed} couldn't be moved to the Recycle Bin`);
    else toast.success(`Moved ${r.deleted} item${r.deleted === 1 ? "" : "s"} to the Recycle Bin`);
    load(path, { push: false });
  };

  // Keyboard: Enter opens, Delete trashes, Backspace goes up, Ctrl+A selects everything
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (viewer !== null || renaming || confirmDelete) return;
      const t = e.target as HTMLElement;
      if (t.tagName === "INPUT" || t.tagName === "TEXTAREA") return;
      if (e.key === "Backspace") { e.preventDefault(); load(parentOf(path)); }
      else if (e.key === "Delete" && sel.length) { e.preventDefault(); setConfirmDelete(sel.map((s) => s.path)); }
      else if (e.key === "Enter" && sel.length === 1) { e.preventDefault(); open(sel[0]); }
      else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") { e.preventDefault(); setSelected(new Set(visible.map((v) => v.path))); }
      else if (e.key === "Escape") setSelected(new Set());
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const isBookmarked = bookmarks.some((b) => b.path.toLowerCase() === path.toLowerCase());
  const cell = SIZES[size];

  return (
    <div ref={rootRef} className="flex min-h-0 flex-1">
      {/* Places */}
      {showPlaces && (
        <aside className="flex w-56 shrink-0 flex-col gap-4 overflow-y-auto border-r bg-sidebar/40 p-3">
          <PlaceGroup title="Places" items={bookmarks} current={path} onOpen={(p) => load(p)} icon={BookmarkIcon}
            onRemove={(p) => call("appBridge", "removeGalleryBookmark", p)} />
          <PlaceGroup title="Drives" items={drives} current={path} onOpen={(p) => load(p)} icon={HardDriveIcon} />
        </aside>
      )}

      <div className="flex min-w-0 flex-1 flex-col">
        {/* Toolbar */}
        <div className="flex shrink-0 flex-col gap-3 border-b px-5 pt-4 pb-3">
          <div className="flex items-center gap-1.5">
            {!showPlaces && (
              <DropdownMenu>
                <DropdownMenuTrigger asChild><Button variant="outline" size="sm" className="mr-1"><BookmarkIcon />Places</Button></DropdownMenuTrigger>
                <DropdownMenuContent align="start" className="max-h-96 w-60 overflow-y-auto">
                  {bookmarks.length > 0 && <DropdownMenuLabel>Places</DropdownMenuLabel>}
                  {bookmarks.map((b) => <DropdownMenuItem key={b.path} onSelect={() => load(b.path)}><BookmarkIcon />{b.name}</DropdownMenuItem>)}
                  {drives.length > 0 && <DropdownMenuSeparator />}
                  {drives.length > 0 && <DropdownMenuLabel>Drives</DropdownMenuLabel>}
                  {drives.map((d) => (
                    <DropdownMenuItem key={d.path} onSelect={() => load(d.path)}><HardDriveIcon />{d.name}
                      {d.free_str && <span className="ml-auto text-xs text-muted-foreground">{d.free_str}</span>}
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>
            )}
            <Tip content="Back"><Button variant="ghost" size="icon-sm" disabled={!back.current.length} onClick={() => { const p = back.current.pop(); if (p) load(p, { push: false }); }}><ChevronLeftIcon /></Button></Tip>
            <Tip content="Up a folder"><Button variant="ghost" size="icon-sm" onClick={() => load(parentOf(path))}><ArrowUpIcon /></Button></Tip>
            <Tip content="Refresh"><Button variant="ghost" size="icon-sm" onClick={() => load(path, { push: false })}>{loading ? <Loader2Icon className="animate-spin" /> : <RefreshCwIcon />}</Button></Tip>
            <Breadcrumbs crumbs={crumbs} max={rootWidth >= 1400 ? 6 : rootWidth >= 1050 ? 4 : 3} onOpen={(p) => load(p)} />
            <Tip content={isBookmarked ? "Remove from Places" : "Add to Places"}>
              <Button variant="ghost" size="icon-sm" onClick={() => call("appBridge", isBookmarked ? "removeGalleryBookmark" : "addGalleryBookmark", path)}>
                {isBookmarked ? <BookmarkIcon className="fill-current" /> : <BookmarkPlusIcon />}
              </Button>
            </Tip>
            <DropdownMenu>
              <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm"><EllipsisIcon /></Button></DropdownMenuTrigger>
              <DropdownMenuContent>
                <DropdownMenuItem onSelect={() => call("appBridge", "openFolder", path)}><FolderOpenIcon />Open in Explorer</DropdownMenuItem>
                <DropdownMenuItem onSelect={() => { clipboard.write(path); toast.success("Path copied"); }}><CopyIcon />Copy folder path</DropdownMenuItem>
                <DropdownMenuItem onSelect={() => call("appBridge", "showDownloadsInGallery")}><DownloadIcon />Go to latest download</DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <div className="relative w-full max-w-64 min-w-40 flex-1">
              <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input className="h-8 pl-8" placeholder="Filter this folder…" value={query} onChange={(e) => setQuery(e.target.value)} />
            </div>
            <Segmented value={kind} onChange={setKind} options={[
              { value: "all", label: "All" }, { value: "image", label: "Images" }, { value: "video", label: "Videos" }, { value: "dir", label: "Folders" },
            ]} />
            <Select size="sm" className="w-32" value={sort} onValueChange={setSort} options={[
              { value: "name", label: "Name" }, { value: "newest", label: "Newest" }, { value: "oldest", label: "Oldest" }, { value: "largest", label: "Largest" },
            ]} />
            <div className="ml-auto flex items-center gap-2">
              {view === "grid" && <Segmented value={size} onChange={setSize} options={[{ value: "s", label: "S" }, { value: "m", label: "M" }, { value: "l", label: "L" }]} />}
              <Segmented value={view} onChange={setView} options={[
                { value: "grid", label: <LayoutGridIcon className="size-3.5" /> }, { value: "list", label: <ListIcon className="size-3.5" /> },
              ]} />
            </div>
          </div>
        </div>

        {/* Items */}
        <div className="relative min-h-0 flex-1 overflow-y-auto" onClick={(e) => { if (e.target === e.currentTarget) setSelected(new Set()); }}>
          {visible.length === 0 && !loading ? (
            <Empty icon={FolderIcon} title={entries.length ? "Nothing matches" : "This folder is empty"} description={entries.length ? "Try another filter." : undefined} />
          ) : view === "grid" ? (
            <div className="flex flex-col gap-6 p-5" onClick={(e) => { if (e.target === e.currentTarget) setSelected(new Set()); }}>
              {folderCount > 0 && (
                <section>
                  <h3 className="mb-2 text-xs font-medium text-muted-foreground">Folders <span className="tabular-nums">{folderCount}</span></h3>
                  <div className="grid gap-2" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))" }}>
                    {visible.slice(0, folderCount).map((e, i) => (
                      <FolderCard key={e.path} e={e} stats={stats[e.path.toLowerCase()]} selected={selected.has(e.path)} onClick={(ev) => click(e, i, ev)} onOpen={() => open(e)} />
                    ))}
                  </div>
                </section>
              )}
              {visible.length > folderCount && (
                <section>
                  {folderCount > 0 && <h3 className="mb-2 text-xs font-medium text-muted-foreground">Files <span className="tabular-nums">{visible.length - folderCount}</span></h3>}
                  <div className="grid gap-3" style={{ gridTemplateColumns: `repeat(auto-fill, minmax(${cell}px, 1fr))` }}>
                    {visible.slice(folderCount).map((e, j) => (
                      <Tile key={e.path} e={e} selected={selected.has(e.path)} onClick={(ev) => click(e, folderCount + j, ev)} onOpen={() => open(e)} />
                    ))}
                  </div>
                </section>
              )}
            </div>
          ) : (
            <div className="py-1">
              {visible.map((e, i) => (
                <ListRow key={e.path} e={e} stats={stats[e.path.toLowerCase()]} selected={selected.has(e.path)} onClick={(ev) => click(e, i, ev)} onOpen={() => open(e)} />
              ))}
            </div>
          )}
          {totalInFolder > entries.length && (
            <div className="px-5 pb-5 text-xs text-muted-foreground">Showing the first {entries.length.toLocaleString()} of {totalInFolder.toLocaleString()} items.</div>
          )}
        </div>

        {/* Selection bar */}
        {sel.length > 0 && (
          <div className="flex shrink-0 items-center gap-2 border-t bg-sidebar px-5 py-2">
            <span className="text-sm font-medium">{sel.length} selected</span>
            <Button variant="ghost" size="xs" onClick={() => setSelected(new Set())}><XIcon />Clear</Button>
            <div className="ml-auto flex items-center gap-1.5">
              {sel.length === 1 && <Button variant="outline" size="sm" onClick={() => open(sel[0])}><PlayIcon />Open</Button>}
              {sel.length === 1 && <Button variant="outline" size="sm" onClick={() => call("appBridge", "revealFileInExplorer", sel[0].path)}><FolderOpenIcon />Show in Explorer</Button>}
              {sel.length === 1 && <Button variant="outline" size="sm" onClick={() => setRenaming(sel[0])}><PencilIcon />Rename</Button>}
              <Button variant="outline" size="sm" onClick={moveSelected}><FolderInputIcon />Move…</Button>
              <Button variant="outline" size="sm" className="text-destructive" onClick={() => setConfirmDelete(sel.map((s) => s.path))}><TrashIcon />Delete</Button>
            </div>
          </div>
        )}
      </div>

      {viewer !== null && media[viewer] && (
        <Viewer items={media} index={viewer} onIndex={setViewer} onClose={() => setViewer(null)}
          onDelete={(p) => { setViewer(null); setConfirmDelete([p]); }} />
      )}
      <RenameDialog entry={renaming} onClose={(changed) => { setRenaming(null); if (changed) load(path, { push: false }); }} />
      <Dialog open={!!confirmDelete} onOpenChange={(o) => !o && setConfirmDelete(null)}>
        <DialogContent title={`Delete ${confirmDelete?.length ?? 0} item${confirmDelete?.length === 1 ? "" : "s"}?`} description="They go to the Recycle Bin, so you can restore them.">
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmDelete(null)}>Cancel</Button>
            <Button variant="destructive" onClick={() => { const p = confirmDelete ?? []; setConfirmDelete(null); deletePaths(p); }}><TrashIcon />Move to Recycle Bin</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

/** Drive first, a "…" menu for the folders in between, then the last few folders. */
function Breadcrumbs({ crumbs, max, onOpen }: { crumbs: Place[]; max: number; onOpen: (path: string) => void }) {
  const collapse = crumbs.length > max;
  const head = collapse ? crumbs.slice(0, 1) : crumbs;
  const hidden = collapse ? crumbs.slice(1, crumbs.length - (max - 1)) : [];
  const tail = collapse ? crumbs.slice(crumbs.length - (max - 1)) : [];
  const Sep = () => <ChevronRightIcon className="mx-0.5 size-3.5 shrink-0 text-muted-foreground/60" />;
  const Crumb = ({ c, last }: { c: Place; last: boolean }) => (
    <button onClick={() => onOpen(c.path)} title={c.path}
      className={cn("min-w-0 truncate rounded px-1.5 py-0.5 hover:bg-accent", last ? "font-medium" : "max-w-40 text-muted-foreground")}>
      {c.name}
    </button>
  );
  const all = [...head, ...tail];
  return (
    <nav className="ml-1 flex min-w-0 flex-1 items-center overflow-hidden text-sm">
      {head.map((c, i) => (
        <span key={c.path} className="flex min-w-0 items-center">
          {i > 0 && <Sep />}<Crumb c={c} last={!collapse && i === head.length - 1} />
        </span>
      ))}
      {collapse && (
        <span className="flex items-center">
          <Sep />
          <DropdownMenu>
            <DropdownMenuTrigger asChild><button className="rounded px-1.5 py-0.5 text-muted-foreground hover:bg-accent">…</button></DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="max-w-80">
              {hidden.map((c) => <DropdownMenuItem key={c.path} onSelect={() => onOpen(c.path)}><FolderIcon /><span className="truncate">{c.name}</span></DropdownMenuItem>)}
            </DropdownMenuContent>
          </DropdownMenu>
        </span>
      )}
      {tail.map((c, i) => (
        <span key={c.path} className="flex min-w-0 items-center">
          <Sep /><Crumb c={c} last={i === tail.length - 1} />
        </span>
      ))}
      {all.length === 0 && <span className="px-1.5 text-muted-foreground">…</span>}
    </nav>
  );
}

function PlaceGroup({ title, items, current, onOpen, onRemove, icon: Icon }: {
  title: string; items: Place[]; current: string; onOpen: (p: string) => void; onRemove?: (p: string) => void; icon: typeof BookmarkIcon;
}) {
  if (!items.length) return null;
  return (
    <div className="flex flex-col gap-0.5">
      <div className="px-2 pb-1 text-xs font-medium text-muted-foreground/70">{title}</div>
      {items.map((p) => (
        <div key={p.path} className="group relative">
          <button onClick={() => onOpen(p.path)} title={p.path}
            className={cn("flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-sm",
              current.toLowerCase() === p.path.toLowerCase() ? "bg-accent font-medium" : "text-muted-foreground hover:bg-accent/60 hover:text-foreground")}>
            <Icon className="size-4 shrink-0" />
            <span className="min-w-0 flex-1 truncate">{p.name}</span>
            {p.free_str && <span className="text-[10px] text-muted-foreground">{p.free_str}</span>}
          </button>
          {onRemove && (
            <button onClick={() => onRemove(p.path)} className="absolute top-1/2 right-1 hidden -translate-y-1/2 rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground group-hover:block">
              <XIcon className="size-3" />
            </button>
          )}
        </div>
      ))}
    </div>
  );
}

function KindIcon({ e, className }: { e: Entry; className?: string }) {
  const k = kindOf(e);
  const Icon = k === "dir" ? FolderIcon : k === "image" ? ImageIcon : k === "video" ? FilmIcon : k === "audio" ? MusicIcon : FileIcon;
  return <Icon className={className} />;
}

function Thumb({ e }: { e: Entry }) {
  const [failed, setFailed] = useState(false);
  const k = kindOf(e);
  if (k === "dir" || failed || (k !== "image" && k !== "video")) {
    return (
      <div className="flex size-full items-center justify-center bg-muted/40">
        <KindIcon e={e} className={cn("size-8", k === "dir" ? "fill-muted-foreground/20 text-muted-foreground" : "text-muted-foreground/70")} />
      </div>
    );
  }
  return <img src={mediaUrl("thumb", e.path)} loading="lazy" decoding="async" draggable={false} onError={() => setFailed(true)} className="size-full object-cover" alt="" />;
}

function FolderCard({ e, stats, selected, onClick, onOpen }: { e: Entry; stats?: FolderStats; selected: boolean; onClick: (ev: React.MouseEvent) => void; onOpen: () => void }) {
  return (
    <button data-path={e.path} onClick={onClick} onDoubleClick={onOpen}
      className={cn("flex min-w-0 items-center gap-3 rounded-lg border px-3 py-2.5 text-left transition-colors",
        selected ? "border-primary/70 bg-accent ring-2 ring-primary/30" : "bg-card hover:border-foreground/20 hover:bg-accent/40")}>
      <FolderIcon className="size-5 shrink-0 fill-muted-foreground/20 text-muted-foreground" />
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm font-medium" title={e.name}>{e.name}</div>
        <div className="h-4 truncate text-xs text-muted-foreground">{folderSummary(e, stats)}</div>
      </div>
    </button>
  );
}

function Tile({ e, selected, onClick, onOpen }: { e: Entry; selected: boolean; onClick: (ev: React.MouseEvent) => void; onOpen: () => void }) {
  const k = kindOf(e);
  return (
    <button data-path={e.path} onClick={onClick} onDoubleClick={onOpen}
      className={cn("group flex min-w-0 flex-col overflow-hidden rounded-lg border text-left transition-colors [content-visibility:auto] [contain-intrinsic-size:auto_220px]",
        selected ? "border-primary/70 ring-2 ring-primary/40" : "hover:border-foreground/20")}>
      <div className="relative aspect-square overflow-hidden bg-muted">
        <Thumb e={e} />
        {k === "video" && <span className="absolute right-1.5 bottom-1.5 flex size-6 items-center justify-center rounded-full bg-black/60 text-white"><PlayIcon className="size-3 fill-current" /></span>}
      </div>
      <div className="px-2 py-1.5">
        <div className="truncate text-xs font-medium" title={e.name}>{e.name}</div>
        <div className="truncate text-[11px] text-muted-foreground">{formatBytes(e.size)}</div>
      </div>
    </button>
  );
}

function ListRow({ e, stats, selected, onClick, onOpen }: { e: Entry; stats?: FolderStats; selected: boolean; onClick: (ev: React.MouseEvent) => void; onOpen: () => void }) {
  return (
    <button data-path={e.path} onClick={onClick} onDoubleClick={onOpen}
      className={cn("flex w-full items-center gap-3 px-5 py-1.5 text-left text-sm [content-visibility:auto] [contain-intrinsic-size:auto_36px]",
        selected ? "bg-accent" : "hover:bg-accent/40")}>
      <KindIcon e={e} className={cn("size-4 shrink-0 text-muted-foreground", e.is_dir && "fill-muted-foreground/20")} />
      <span className="min-w-0 flex-1 truncate">{e.name}</span>
      <span className="w-44 truncate text-right text-xs text-muted-foreground tabular-nums">{e.is_dir ? folderSummary(e, stats) : formatBytes(e.size)}</span>
      <span className="hidden w-36 text-right text-xs text-muted-foreground tabular-nums @3xl/main:block">{e.mtime ? new Date(e.mtime * 1000).toLocaleString() : ""}</span>
    </button>
  );
}

function Viewer({ items, index, onIndex, onClose, onDelete }: {
  items: Entry[]; index: number; onIndex: (i: number) => void; onClose: () => void; onDelete: (path: string) => void;
}) {
  const e = items[index];
  const [broken, setBroken] = useState(false);
  useEffect(() => setBroken(false), [e.path]);
  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      // ← → move between items (the video player takes them for seeking while a video is open);
      // Page Up / Page Down always move
      const next = ev.key === "ArrowRight" || ev.key === "PageDown";
      const prev = ev.key === "ArrowLeft" || ev.key === "PageUp";
      if (ev.key === "Escape" && !document.fullscreenElement) onClose();
      else if (next && index < items.length - 1) onIndex(index + 1);
      else if (prev && index > 0) onIndex(index - 1);
      else if (ev.key === "Delete") onDelete(e.path);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [index, items.length, onClose, onIndex, onDelete, e.path]);

  const k = kindOf(e);
  return (
    <div className="fixed inset-0 z-50 flex flex-col bg-neutral-950 text-white">
      <div className="flex shrink-0 items-center gap-3 px-4 py-3">
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm font-medium">{e.name}</div>
          <div className="text-xs text-white/60 tabular-nums">{index + 1} of {items.length} · {formatBytes(e.size)}</div>
        </div>
        <Tip content="Open with the default app"><Button variant="ghost" size="icon-sm" className="text-white hover:bg-white/10 hover:text-white" onClick={() => call("appBridge", "openPathInSystem", e.path)}><ExternalLinkIcon /></Button></Tip>
        <Tip content="Show in Explorer"><Button variant="ghost" size="icon-sm" className="text-white hover:bg-white/10 hover:text-white" onClick={() => call("appBridge", "revealFileInExplorer", e.path)}><FolderOpenIcon /></Button></Tip>
        <Tip content="Delete"><Button variant="ghost" size="icon-sm" className="text-white hover:bg-white/10 hover:text-white" onClick={() => onDelete(e.path)}><TrashIcon /></Button></Tip>
        <DropdownMenu>
          <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm" className="text-white hover:bg-white/10 hover:text-white"><EllipsisIcon /></Button></DropdownMenuTrigger>
          <DropdownMenuContent>
            <DropdownMenuLabel>{e.name}</DropdownMenuLabel>
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={() => { clipboard.write(e.path); toast.success("Path copied"); }}><CopyIcon />Copy path</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <Tip content="Close (Esc)"><Button variant="ghost" size="icon-sm" className="text-white hover:bg-white/10 hover:text-white" onClick={onClose}><XIcon /></Button></Tip>
      </div>
      <div className="relative flex min-h-0 flex-1 items-center justify-center px-16 pb-6">
        {broken ? (
          <div className="flex flex-col items-center gap-3 text-sm text-white/70">
            <FileIcon className="size-8" />This file can't be shown here.
            <Button variant="secondary" size="sm" onClick={() => call("appBridge", "openPathInSystem", e.path)}><ExternalLinkIcon />Open with the default app</Button>
          </div>
        ) : k === "video" ? (
          <MediaPlayer key={e.path} src={mediaUrl("file", e.path)} onError={() => setBroken(true)} className="h-full w-full" />
        ) : (
          <img key={e.path} src={mediaUrl("image", e.path)} draggable={false} className="max-h-full max-w-full object-contain" onError={() => setBroken(true)} alt={e.name} />
        )}
        {index > 0 && (
          <button onClick={() => onIndex(index - 1)} className="absolute top-1/2 left-3 flex size-10 -translate-y-1/2 items-center justify-center rounded-full bg-white/10 hover:bg-white/20">
            <ChevronLeftIcon className="size-5" />
          </button>
        )}
        {index < items.length - 1 && (
          <button onClick={() => onIndex(index + 1)} className="absolute top-1/2 right-3 flex size-10 -translate-y-1/2 items-center justify-center rounded-full bg-white/10 hover:bg-white/20">
            <ChevronRightIcon className="size-5" />
          </button>
        )}
      </div>
    </div>
  );
}

function RenameDialog({ entry, onClose }: { entry: Entry | null; onClose: (changed: boolean) => void }) {
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  useEffect(() => { setName(entry?.name ?? ""); setError(""); }, [entry]);
  const submit = async () => {
    if (!entry) return;
    const r = await call<{ success: boolean; error: string }>("appBridge", "renameItem", entry.path, name.trim());
    if (r.success) { toast.success("Renamed"); onClose(true); } else setError(r.error || "Couldn't rename it");
  };
  return (
    <Dialog open={!!entry} onOpenChange={(o) => !o && onClose(false)}>
      <DialogContent title="Rename" description={entry?.name}>
        <Input autoFocus value={name} onChange={(e) => { setName(e.target.value); setError(""); }}
          onFocus={(e) => { const dot = e.target.value.lastIndexOf("."); e.target.setSelectionRange(0, dot > 0 && !entry?.is_dir ? dot : e.target.value.length); }}
          onKeyDown={(e) => { if (e.key === "Enter" && name.trim()) submit(); }} className={cn(error && "border-destructive/60")} />
        {error && <p className="text-xs text-destructive">{error}</p>}
        <DialogFooter>
          <Button variant="outline" onClick={() => onClose(false)}>Cancel</Button>
          <Button disabled={!name.trim() || name === entry?.name} onClick={submit}>Rename</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
