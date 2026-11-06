import { useEffect, useMemo, useRef, useState } from "react";
import {
  ArchiveIcon, ArrowLeftIcon, CheckIcon, DownloadIcon, FileIcon, FilmIcon, ImageIcon, ImageOffIcon, ListPlusIcon, MusicIcon, SearchIcon,
} from "lucide-react";
import { call, useSignal } from "@/lib/engine";
import { Badge, Button, Dialog, DialogContent, DialogFooter, Input } from "@/components/ui";
import { cn, formatBytes } from "@/lib/utils";

type PostFile = { name: string; path?: string; thumbnail?: string; previewUrl?: string; videoUrl?: string; size?: number; is_main?: boolean };
type Post = { id: string; title: string; published?: string; thumbnail?: string; fileCount?: number; files?: PostFile[] };

const VIDEO = /\.(mp4|m4v|webm|mov|mkv|avi|wmv|flv|gifv)$/i;
const IMAGE = /\.(jpe?g|png|gif|webp|avif|bmp|jfif|heic|tiff?)$/i;
const AUDIO = /\.(mp3|flac|wav|ogg|opus|m4a|aac)$/i;
const ARCHIVE = /\.(zip|rar|7z|tar|gz|bz2|xz)$/i;
const fileKey = (f: PostFile) => f.path || f.name;
const extOf = (n: string) => (n.match(/\.([a-z0-9]+)$/i)?.[1] ?? "").toUpperCase();

function FileTypeIcon({ name, className }: { name: string; className?: string }) {
  const Icon = VIDEO.test(name) ? FilmIcon : IMAGE.test(name) ? ImageIcon : AUDIO.test(name) ? MusicIcon : ARCHIVE.test(name) ? ArchiveIcon : FileIcon;
  return <Icon className={className} />;
}

/**
 * A still frame of a remote video. The video only starts loading once it scrolls into view, and
 * then only its metadata and the frame at ~1s (a few small range requests, not the whole file).
 */
function VideoFrame({ src, fallback }: { src: string; fallback: React.ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);
  const [failed, setFailed] = useState(false);
  const [ready, setReady] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const io = new IntersectionObserver(([e]) => { if (e.isIntersecting) { setVisible(true); io.disconnect(); } }, { rootMargin: "200px" });
    io.observe(el);
    return () => io.disconnect();
  }, []);
  return (
    <div ref={ref} className="relative flex size-full items-center justify-center">
      {(!ready || failed) && fallback}
      {visible && !failed && (
        <video src={`${src}#t=1`} preload="metadata" muted playsInline disablePictureInPicture
          onLoadedData={() => setReady(true)} onError={() => setFailed(true)}
          className={cn("absolute inset-0 size-full object-cover", !ready && "invisible")} />
      )}
    </div>
  );
}

function Check({ on }: { on: boolean }) {
  return (
    <span className={cn("flex size-5 items-center justify-center rounded border shadow-sm", on ? "border-primary bg-primary text-primary-foreground" : "border-white/60 bg-black/40")}>
      {on && <CheckIcon className="size-3.5" />}
    </span>
  );
}

/**
 * Opens when the engine finishes "Select posts…". Pick posts, and inside any post pick the exact
 * files you want (any type); then download or queue them.
 */
export function PostSelectionDialog() {
  const [open, setOpen] = useState(false);
  const [posts, setPosts] = useState<Post[]>([]);
  const [creator, setCreator] = useState("");
  // Selection is by card (index): revisions of a post come back as separate cards with the same
  // post id, so tracking ids made clicking one card flip all of its twins
  const [selected, setSelected] = useState<Set<number>>(new Set());
  // Files left out, per card; a card with nothing left out downloads all of its files
  const [excluded, setExcluded] = useState<Map<number, Set<string>>>(new Map());
  const [viewing, setViewing] = useState<number | null>(null);
  const [query, setQuery] = useState("");
  const anchor = useRef(-1);

  useSignal("appBridge", "postSelectionReady", (cards: Post[], name: string) => {
    const list = Array.isArray(cards) ? cards : [];
    setPosts(list);
    setCreator(String(name || ""));
    setSelected(new Set(list.map((_, i) => i)));
    setExcluded(new Map());
    // A single post (a post link) goes straight to choosing its files
    setViewing(list.length === 1 && (list[0].files?.length ?? 0) > 0 ? 0 : null);
    anchor.current = -1;
    setQuery("");
    setOpen(true);
  });

  const files = (i: number) => posts[i]?.files ?? [];
  const chosenCount = (i: number) => {
    const fl = files(i);
    if (!fl.length) return posts[i]?.fileCount ?? 0;
    return fl.length - (excluded.get(i)?.size ?? 0);
  };

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    const all = posts.map((p, i) => ({ p, i }));
    return q ? all.filter(({ p }) => (p.title || "").toLowerCase().includes(q)) : all;
  }, [posts, query]);

  const togglePost = (i: number, shift: boolean) => setSelected((s) => {
    const n = new Set(s);
    if (shift && anchor.current >= 0) {
      const on = !s.has(i);
      const [a, b] = [Math.min(anchor.current, i), Math.max(anchor.current, i)];
      for (const { i: k } of visible) if (k >= a && k <= b) { if (on) n.add(k); else n.delete(k); }
    } else if (n.has(i)) n.delete(i); else n.add(i);
    anchor.current = i;
    return n;
  });

  const setFileState = (i: number, keys: string[], on: boolean) => {
    setExcluded((m) => {
      const n = new Map(m);
      const ex = new Set(n.get(i) ?? []);
      keys.forEach((k) => (on ? ex.delete(k) : ex.add(k)));
      n.set(i, ex);
      // The post is selected exactly when at least one of its files is
      setSelected((s) => { const sel = new Set(s); if (ex.size >= files(i).length) sel.delete(i); else sel.add(i); return sel; });
      return n;
    });
  };

  const allVisibleSelected = visible.length > 0 && visible.every(({ i }) => selected.has(i));
  const totalFiles = [...selected].reduce((n, i) => n + chosenCount(i), 0);

  const finish = (autoStart: boolean) => {
    const ids: string[] = [];
    const filesMap: Record<string, string[]> = {};
    let partial = false;
    for (const i of [...selected].sort((a, b) => a - b)) {
      const id = String(posts[i]?.id ?? "");
      if (!id) continue;
      if (!ids.includes(id)) ids.push(id);
      const fl = files(i);
      const ex = excluded.get(i);
      const kept = fl.filter((f) => !ex?.has(fileKey(f))).map(fileKey);
      if (ex?.size) partial = true;
      filesMap[id] = [...new Set([...(filesMap[id] ?? []), ...kept])];
    }
    call("appBridge", "startDownloadSelectedPosts", ids, autoStart, partial ? filesMap : null);
    setOpen(false);
  };

  const viewPost = viewing !== null ? posts[viewing] : null;

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) call("appBridge", "cancelPostSelection").catch(() => {}); setOpen(o); }}>
      <DialogContent className="flex h-[80vh] max-w-4xl flex-col"
        title={viewPost ? (viewPost.title || "Untitled post") : creator ? `Select posts — ${creator}` : "Select posts"}
        description={viewPost
          ? `${chosenCount(viewing!)} of ${files(viewing!).length} files chosen`
          : `${posts.length} posts found · ${selected.size} selected · ${totalFiles} files`}>
        {viewPost && viewing !== null ? (
          <>
            <div className="flex items-center gap-2">
              {posts.length > 1 && <Button variant="ghost" size="sm" className="-ml-2" onClick={() => setViewing(null)}><ArrowLeftIcon />All posts</Button>}
              <div className="ml-auto flex gap-2">
                <Button variant="outline" size="sm" onClick={() => setFileState(viewing, files(viewing).map(fileKey), true)}>Select all</Button>
                <Button variant="outline" size="sm" onClick={() => setFileState(viewing, files(viewing).map(fileKey), false)}>Select none</Button>
              </div>
            </div>
            <div className="-mx-1 grid min-h-0 flex-1 auto-rows-max grid-cols-2 gap-3 overflow-y-auto px-1 md:grid-cols-3 lg:grid-cols-4">
              {files(viewing).map((f) => {
                const key = fileKey(f);
                const on = !excluded.get(viewing)?.has(key);
                const thumb = f.thumbnail || f.previewUrl;
                return (
                  <button key={key} onClick={() => setFileState(viewing, [key], !on)}
                    className={cn("relative flex flex-col overflow-hidden rounded-lg border text-left transition-colors", on ? "border-primary/70 ring-1 ring-primary/40" : "opacity-60 hover:opacity-100")}>
                    <div className="relative flex aspect-[4/3] items-center justify-center bg-muted">
                      {thumb ? <img src={thumb} loading="lazy" className="size-full object-cover" alt="" />
                        : f.videoUrl ? <VideoFrame src={f.videoUrl} fallback={<FileTypeIcon name={f.name} className="size-8 text-muted-foreground" />} />
                        : <FileTypeIcon name={f.name} className="size-8 text-muted-foreground" />}
                      <span className="absolute top-2 left-2"><Check on={on} /></span>
                      {extOf(f.name) && <Badge variant="secondary" className="absolute right-2 bottom-2 bg-black/65 font-mono text-[10px] text-white">{extOf(f.name)}</Badge>}
                    </div>
                    <div className="p-2.5">
                      <div className="line-clamp-2 text-[13px] font-medium break-all">{f.name}</div>
                      {f.size ? <div className="mt-0.5 text-xs text-muted-foreground">{formatBytes(f.size)}</div> : null}
                    </div>
                  </button>
                );
              })}
            </div>
          </>
        ) : (
          <>
            <div className="flex items-center gap-2">
              <div className="relative flex-1">
                <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input className="h-8 pl-8" placeholder="Filter by title…" value={query} onChange={(e) => setQuery(e.target.value)} />
              </div>
              <Button variant="outline" size="sm" onClick={() => setSelected((s) => {
                const n = new Set(s);
                visible.forEach(({ i }) => (allVisibleSelected ? n.delete(i) : n.add(i)));
                return n;
              })}>{allVisibleSelected ? "Deselect all" : "Select all"}</Button>
            </div>
            <div className="-mx-1 grid min-h-0 flex-1 auto-rows-max grid-cols-2 gap-3 overflow-y-auto px-1 md:grid-cols-3 lg:grid-cols-4">
              {visible.map(({ p, i }) => {
                const on = selected.has(i);
                const total = files(i).length || p.fileCount || 0;
                const chosen = chosenCount(i);
                const cover = !p.thumbnail ? files(i).find((f) => f.videoUrl)?.videoUrl : undefined;
                const noCover = <div className="flex size-full items-center justify-center"><ImageOffIcon className="size-5 text-muted-foreground" /></div>;
                return (
                  <div key={i} role="button" tabIndex={0} onClick={(e) => togglePost(i, e.shiftKey)}
                    onKeyDown={(e) => { if (e.key === " " || e.key === "Enter") { e.preventDefault(); togglePost(i, e.shiftKey); } }}
                    className={cn("group relative flex cursor-default flex-col overflow-hidden rounded-lg border text-left transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
                      on ? "border-primary/70 ring-1 ring-primary/40" : "opacity-70 hover:opacity-100")}>
                    <div className="relative aspect-[4/3] bg-muted">
                      {p.thumbnail ? <img src={p.thumbnail} loading="lazy" className="size-full object-cover" alt="" />
                        : cover ? <VideoFrame src={cover} fallback={<FilmIcon className="size-5 text-muted-foreground" />} />
                        : noCover}
                      <span className="absolute top-2 left-2"><Check on={on} /></span>
                      {total > 0 && (
                        <button onClick={(e) => { e.stopPropagation(); if (files(i).length) setViewing(i); }}
                          className={cn("absolute right-2 bottom-2 rounded-md bg-black/70 px-2 py-0.5 text-xs font-medium text-white transition-colors",
                            files(i).length ? "hover:bg-black/90" : "cursor-default")}
                          title={files(i).length ? "Choose files" : undefined}>
                          {chosen < total ? `${chosen}/${total} files` : `${total} file${total === 1 ? "" : "s"}`}
                        </button>
                      )}
                    </div>
                    <div className="p-2.5">
                      <div className="line-clamp-2 text-[13px] font-medium">{p.title || "Untitled"}</div>
                      {p.published && <div className="mt-0.5 text-xs text-muted-foreground">{String(p.published).slice(0, 10)}</div>}
                    </div>
                  </div>
                );
              })}
            </div>
          </>
        )}
        <DialogFooter>
          <Button variant="outline" disabled={!totalFiles && !selected.size} onClick={() => finish(false)}><ListPlusIcon />Add to queue</Button>
          <Button disabled={!totalFiles && !selected.size} onClick={() => finish(true)}>
            <DownloadIcon />Download {totalFiles ? `${totalFiles} file${totalFiles === 1 ? "" : "s"}` : selected.size}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
