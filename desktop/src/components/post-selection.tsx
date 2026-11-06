import { useMemo, useRef, useState } from "react";
import { CheckIcon, DownloadIcon, ImageOffIcon, ListPlusIcon, SearchIcon } from "lucide-react";
import { call, useSignal } from "@/lib/engine";
import { Badge, Button, Dialog, DialogContent, DialogFooter, Input } from "@/components/ui";
import { cn } from "@/lib/utils";

type Post = { id: string; title: string; published?: string; thumbnail?: string; fileCount?: number };

/** Opens when the engine finishes "Select posts…": pick posts, then download or queue them. */
export function PostSelectionDialog() {
  const [open, setOpen] = useState(false);
  const [posts, setPosts] = useState<Post[]>([]);
  const [creator, setCreator] = useState("");
  // Selection is by card (index): revisions of a post come back as separate cards with the same
  // post id, so tracking ids made clicking one card flip all of its twins
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [query, setQuery] = useState("");
  const anchor = useRef(-1);

  useSignal("appBridge", "postSelectionReady", (cards: Post[], name: string) => {
    const list = Array.isArray(cards) ? cards : [];
    setPosts(list);
    setCreator(String(name || ""));
    setSelected(new Set(list.map((_, i) => i)));
    anchor.current = -1;
    setQuery("");
    setOpen(true);
  });

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    const all = posts.map((p, i) => ({ p, i }));
    return q ? all.filter(({ p }) => (p.title || "").toLowerCase().includes(q)) : all;
  }, [posts, query]);

  const toggle = (i: number, shift: boolean) => setSelected((s) => {
    const n = new Set(s);
    if (shift && anchor.current >= 0) {
      // Shift-click: give the whole range the state of the clicked card's new value
      const on = !s.has(i);
      const [a, b] = [Math.min(anchor.current, i), Math.max(anchor.current, i)];
      for (const { i: k } of visible) if (k >= a && k <= b) { if (on) n.add(k); else n.delete(k); }
    } else if (n.has(i)) n.delete(i); else n.add(i);
    anchor.current = i;
    return n;
  });
  const allVisibleSelected = visible.length > 0 && visible.every(({ i }) => selected.has(i));

  const finish = (autoStart: boolean) => {
    const ids = [...new Set([...selected].map((i) => String(posts[i]?.id ?? "")).filter(Boolean))];
    call("appBridge", "startDownloadSelectedPosts", ids, autoStart);
    setOpen(false);
  };

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) call("appBridge", "cancelPostSelection").catch(() => {}); setOpen(o); }}>
      <DialogContent className="flex h-[80vh] max-w-4xl flex-col" title={creator ? `Select posts — ${creator}` : "Select posts"}
        description={`${posts.length} posts found. ${selected.size} selected.`}>
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
            return (
              <button key={i} onClick={(e) => toggle(i, e.shiftKey)}
                className={cn("group relative flex flex-col overflow-hidden rounded-lg border text-left transition-colors", on ? "border-primary/70 ring-1 ring-primary/40" : "opacity-70 hover:opacity-100")}>
                <div className="relative aspect-[4/3] bg-muted">
                  {p.thumbnail ? <img src={p.thumbnail} loading="lazy" className="size-full object-cover" alt="" />
                    : <div className="flex size-full items-center justify-center"><ImageOffIcon className="size-5 text-muted-foreground" /></div>}
                  <span className={cn("absolute top-2 left-2 flex size-5 items-center justify-center rounded border", on ? "border-primary bg-primary text-primary-foreground" : "border-white/50 bg-black/40")}>
                    {on && <CheckIcon className="size-3.5" />}
                  </span>
                  {p.fileCount ? <Badge variant="secondary" className="absolute right-2 bottom-2 bg-black/60 text-white">{p.fileCount} files</Badge> : null}
                </div>
                <div className="p-2.5">
                  <div className="line-clamp-2 text-[13px] font-medium">{p.title || "Untitled"}</div>
                  {p.published && <div className="mt-0.5 text-xs text-muted-foreground">{String(p.published).slice(0, 10)}</div>}
                </div>
              </button>
            );
          })}
        </div>
        <DialogFooter>
          <Button variant="outline" disabled={!selected.size} onClick={() => finish(false)}><ListPlusIcon />Add to queue</Button>
          <Button disabled={!selected.size} onClick={() => finish(true)}><DownloadIcon />Download {selected.size}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
