import { useMemo, useState, useSyncExternalStore } from "react";
import { DownloadIcon, EllipsisIcon, LayoutGridIcon, Loader2Icon, RefreshCwIcon, SearchIcon, StarIcon } from "lucide-react";
import { toast } from "sonner";
import { call, useProp, useSignal } from "@/lib/engine";
import {
  Badge, Button, Dialog, DialogContent, DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger, Empty, Input,
} from "@/components/ui";

type Fav = { userId: string; service: string; name: string; url: string; provider: string; lastPostDate: string; status: string; newPostCount: number };

let isOpen = false;
const listeners = new Set<() => void>();
export function openFavorites() { isOpen = true; listeners.forEach((l) => l()); call("appBridge", "fetchFavoriteArtists").catch(() => {}); }
const close = () => { isOpen = false; listeners.forEach((l) => l()); };
const useOpen = () => useSyncExternalStore((l) => { listeners.add(l); return () => { listeners.delete(l); }; }, () => isOpen);

/** Creators favorited on your Pawchive / cum.st account. */
export function FavoritesDialog() {
  const open = useOpen();
  const loading = useProp<boolean>("appBridge", "favoriteArtistsLoading", false);
  const [list, setList] = useState<Fav[]>([]);
  const [error, setError] = useState("");
  const [q, setQ] = useState("");

  useSignal("appBridge", "favoriteArtistsReady", (cards: Fav[]) => { setList(Array.isArray(cards) ? cards : []); setError(""); });
  useSignal("appBridge", "favoriteArtistsError", (msg: string) => setError(String(msg)));
  useSignal("appBridge", "favoriteAuthRequired", (provider: string) =>
    setError(`Sign in to ${provider || "your account"} under Settings → Accounts to see your favorites.`));

  const shown = useMemo(() => {
    const s = q.trim().toLowerCase();
    return s ? list.filter((f) => `${f.name} ${f.service}`.toLowerCase().includes(s)) : list;
  }, [list, q]);
  const withNew = list.filter((f) => f.newPostCount > 0).length;

  return (
    <Dialog open={open} onOpenChange={(o) => !o && close()}>
      <DialogContent className="flex h-[78vh] max-w-2xl flex-col" title="Account favorites" description={loading ? "Loading your favorites…" : `${list.length} creators${withNew ? ` · ${withNew} with new posts` : ""}`}>
        <div className="flex items-center gap-2">
          <div className="relative flex-1">
            <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input className="h-8 pl-8" placeholder="Filter…" value={q} onChange={(e) => setQ(e.target.value)} />
          </div>
          <Button variant="outline" size="sm" disabled={loading} onClick={() => call("appBridge", "fetchFavoriteArtists")}>
            {loading ? <Loader2Icon className="animate-spin" /> : <RefreshCwIcon />}Refresh
          </Button>
          <Button size="sm" disabled={!list.length} onClick={() => { call("appBridge", "downloadAllFavoriteArtists"); toast.success("Downloading new posts from all favorites"); close(); }}>
            <DownloadIcon />Get all new
          </Button>
        </div>
        {error && <p className="text-sm text-destructive">{error}</p>}
        <div className="-mx-1 min-h-0 flex-1 overflow-y-auto px-1">
          {shown.length === 0 && !loading ? (
            <Empty icon={StarIcon} title={list.length ? "Nothing matches" : "No favorites yet"} description={list.length ? undefined : "Favorite creators on Pawchive or cum.st and they show up here."} />
          ) : (
            <div className="overflow-hidden rounded-xl border">
              {shown.map((f) => (
                <div key={`${f.service}:${f.userId}`} className="flex items-center gap-3 border-b px-4 py-2.5 last:border-b-0 hover:bg-accent/30">
                  <StarIcon className="size-4 shrink-0 fill-warning text-warning" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">{f.name || f.userId}</div>
                    <div className="truncate text-xs text-muted-foreground"><span className="capitalize">{f.service}</span>{f.lastPostDate ? ` · up to ${String(f.lastPostDate).slice(0, 10)}` : " · not downloaded yet"}</div>
                  </div>
                  {f.newPostCount > 0 && <Badge variant="info">{f.newPostCount} new</Badge>}
                  <Button size="xs" variant="outline" onClick={() => { call("appBridge", "downloadFavoriteArtist", f.userId, f.service); toast.success(`Downloading ${f.name}`); }}>
                    <DownloadIcon />{f.lastPostDate ? "Get new" : "Download"}
                  </Button>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-xs"><EllipsisIcon /></Button></DropdownMenuTrigger>
                    <DropdownMenuContent>
                      <DropdownMenuItem onSelect={() => { call("appBridge", "fetchArtistPostsForSelection", f.userId, f.service, f.name, f.url); close(); }}><LayoutGridIcon />Choose posts…</DropdownMenuItem>
                      <DropdownMenuItem onSelect={() => { call("appBridge", "downloadFavoriteArtist", f.userId, f.service, 1, 999999); toast.success(`Downloading everything from ${f.name}`); }}><DownloadIcon />Download everything</DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              ))}
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
