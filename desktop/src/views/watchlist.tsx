import { useEffect, useState } from "react";
import { BellRingIcon, BookmarkIcon, DownloadIcon, EllipsisIcon, FolderOpenIcon, ListChecksIcon, ListXIcon, Loader2Icon, PlusIcon, RefreshCwIcon, SearchIcon, StarIcon, TrashIcon, TriangleAlertIcon } from "lucide-react";
import { toast } from "sonner";
import { call, useModel, useProp, useQuery, useSetting, useSignal } from "@/lib/engine";
import {
  Alert, Badge, Button, Dialog, DialogContent, DialogFooter, DropdownMenu, DropdownMenuContent, DropdownMenuItem,
  DropdownMenuSeparator, DropdownMenuTrigger, Empty, Input, Label, Segmented, Switch, Tip,
} from "@/components/ui";
import { PageBody, PageHeader } from "@/components/page";
import { cn } from "@/lib/utils";

type Entry = {
  url: string; creatorName: string; service: string; userId: string; lastPostDate: string; addedAt: string;
  autoCheck: boolean; newPostCount: number; downloadDir: string; isFavorite: boolean; provider: string; isOnDisk: boolean;
};

const W = "appBridge.watchlistModel";

function initials(name: string) {
  return (name || "?").split(/\s+/).map((w) => w[0]).join("").slice(0, 2).toUpperCase();
}

export function WatchlistView() {
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState("all");
  const [adding, setAdding] = useState(false);
  const [checking, setChecking] = useState(false);
  const total = useProp<number>(W, "totalCount", 0);
  const updated = useProp<number>(W, "updatedCount", 0);
  const favorites = useProp<number>(W, "favoriteCount", 0);
  const onDisk = useProp<number>(W, "onDiskCount", 0);
  const newPosts = useProp<number>(W, "totalNewPosts", 0);
  const [checkOnStartup, setCheckOnStartup] = useSetting<boolean>("appBridge", "watchlistCheckOnStartup", false);
  const { rows } = useModel<Entry>(W, { throttleMs: 200 });

  useEffect(() => {
    const t = setTimeout(() => call(W, "setFilter", search, filter), 150);
    return () => clearTimeout(t);
  }, [search, filter]);

  useSignal("appBridge", "watchlistCheckStarted", () => setChecking(true));
  useSignal("appBridge", "watchlistCheckFinished", () => setChecking(false));

  const { data: kemonoCount, reload: reloadKemono } = useQuery<number>("appBridge", "kemonoWatchlistCount");
  useSignal("appBridge", "watchlistChanged", () => reloadKemono());

  return (
    <>
      <PageHeader title="Watchlist" description={`${total} creators followed${newPosts ? ` · ${newPosts} new posts` : ""}`}
        actions={<>
          <Button variant="outline" size="sm" disabled={checking || !total} onClick={() => call("appBridge", "checkWatchlist", true)}>
            {checking ? <Loader2Icon className="animate-spin" /> : <RefreshCwIcon />}{checking ? "Checking…" : "Check now"}
          </Button>
          <Button size="sm" onClick={() => setAdding(true)}><PlusIcon />Add creator</Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm"><EllipsisIcon /></Button></DropdownMenuTrigger>
            <DropdownMenuContent className="w-64">
              <DropdownMenuItem onSelect={(e) => { e.preventDefault(); setCheckOnStartup(!checkOnStartup); }}>
                <BellRingIcon />Check when the app starts
                <Switch className="ml-auto" checked={checkOnStartup} tabIndex={-1} />
              </DropdownMenuItem>
              <DropdownMenuItem onSelect={() => call("appBridge", "setAllWatchlistAutoCheck", true)}><ListChecksIcon />Turn on checks for everyone</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => call("appBridge", "disableAllWatchlistAutoCheck")}><ListXIcon />Turn off checks for everyone</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={() => call("appBridge", "syncPawchiveFavorites")}><StarIcon />Import Pawchive favorites</DropdownMenuItem>
              <DropdownMenuItem disabled={!updated} onSelect={() => call("appBridge", "downloadAllNewPosts")}><DownloadIcon />Download all new posts</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </>}>
        <div className="relative w-full max-w-72 min-w-44 flex-1">
          <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input className="h-8 pl-8" placeholder="Search creators…" value={search} onChange={(e) => setSearch(e.target.value)} />
        </div>
        <Segmented value={filter} onChange={setFilter} options={[
          { value: "all", label: "All", count: total },
          { value: "updates", label: "New posts", count: updated },
          { value: "favorites", label: "Favorites", count: favorites },
          { value: "on_disk", label: "On disk", count: onDisk },
        ]} />
      </PageHeader>

      {(updated > 0 || (kemonoCount ?? 0) > 0) && (
        <div className="flex flex-col gap-2 px-6 pt-4">
          {updated > 0 && (
            <Alert tone="info" icon={BellRingIcon}
              action={<Button size="xs" onClick={() => call("appBridge", "downloadAllNewPosts")}><DownloadIcon />Download all</Button>}>
              {newPosts} new post{newPosts === 1 ? "" : "s"} from {updated} creator{updated === 1 ? "" : "s"}.
            </Alert>
          )}
          {(kemonoCount ?? 0) > 0 && (
            <Alert tone="warning" icon={TriangleAlertIcon}
              action={<Button size="xs" variant="outline" onClick={async () => { await call("appBridge", "switchWatchlistToPawchive"); reloadKemono(); }}>Switch to Pawchive</Button>}>
              {kemonoCount} creator{kemonoCount === 1 ? " uses" : "s use"} Kemono, which isn't supported. Pawchive has the same creators.
            </Alert>
          )}
        </div>
      )}

      {rows.length === 0 ? (
        <Empty icon={BookmarkIcon} title={total ? "No creators match" : "No creators followed yet"}
          description={total ? "Try a different search or filter." : "Creators you download are added automatically. You can also add one by link."}>
          {!total && <Button size="sm" onClick={() => setAdding(true)}><PlusIcon />Add creator</Button>}
        </Empty>
      ) : (
        <PageBody>
          <div className="overflow-hidden rounded-xl border">
            {rows.map((e) => <Row key={`${e.service}:${e.userId}`} e={e} />)}
          </div>
        </PageBody>
      )}
      <AddDialog open={adding} onOpenChange={setAdding} />
    </>
  );
}

function Row({ e }: { e: Entry }) {
  return (
    <div className="group flex items-center gap-4 border-b px-4 py-3 last:border-b-0 hover:bg-accent/30">
      <div className="flex size-9 shrink-0 items-center justify-center rounded-full bg-secondary text-xs font-semibold">{initials(e.creatorName)}</div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate font-medium">{e.creatorName || e.userId}</span>
          {e.isFavorite && <StarIcon className="size-3.5 fill-warning text-warning" />}
          {e.newPostCount > 0 && <Badge variant="info"><BellRingIcon />{e.newPostCount} new</Badge>}
        </div>
        <div className="mt-0.5 flex items-center gap-2 text-xs text-muted-foreground">
          <span className="capitalize">{e.service}</span><span>·</span>
          <span>{e.provider === "cumst" ? "cum.st" : "Pawchive"}</span><span>·</span>
          <span>{e.lastPostDate ? `Downloaded up to ${String(e.lastPostDate).slice(0, 10)}` : "Nothing downloaded yet"}</span>
        </div>
      </div>
      <Tip content="Check this creator on startup">
        <span className="flex items-center"><Switch checked={!!e.autoCheck} onCheckedChange={(v) => call("appBridge", "setWatchlistAutoCheck", e.userId, e.service, v)} /></span>
      </Tip>
      {e.newPostCount > 0 && (
        <Button size="sm" variant="outline" onClick={() => call("appBridge", "downloadNewPosts", e.userId, e.service)}><DownloadIcon />Download</Button>
      )}
      <Tip content="Check now">
        <Button size="icon-sm" variant="ghost" onClick={() => call("appBridge", "checkWatchlistArtist", e.userId, e.service)}><RefreshCwIcon /></Button>
      </Tip>
      <DropdownMenu>
        <DropdownMenuTrigger asChild><Button size="icon-sm" variant="ghost"><EllipsisIcon /></Button></DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem onSelect={() => call(W, "toggleFavorite", e.userId, e.service, !e.isFavorite)}>
            <StarIcon />{e.isFavorite ? "Remove from favorites" : "Add to favorites"}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => call("appBridge", "redownloadWatchlistEntry", e.userId, e.service)}><DownloadIcon />Download everything again</DropdownMenuItem>
          {e.downloadDir && <DropdownMenuItem onSelect={() => call("appBridge", "openFolder", e.downloadDir)}><FolderOpenIcon />Open folder</DropdownMenuItem>}
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onSelect={() => call("appBridge", "removeFromWatchlist", e.userId, e.service)}><TrashIcon />Remove</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

function AddDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const blocked = /(kemono|coomer)\./i.test(url);
  const submit = async () => {
    setBusy(true);
    try {
      const ok = await call<boolean>("appBridge", "addArtistToWatchlist", url.trim(), "");
      if (ok) { toast.success("Creator added to the watchlist"); setUrl(""); onOpenChange(false); }
      else toast.error("Couldn't add that link. Use a Pawchive or cum.st creator page.");
    } finally { setBusy(false); }
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent title="Add creator" description="Follow a creator to be told about new posts.">
        <div className="flex flex-col gap-2">
          <Label htmlFor="wl-url">Creator link</Label>
          <Input id="wl-url" autoFocus placeholder="https://pawchive.pw/patreon/user/…" value={url} onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && url.trim() && !blocked) submit(); }} className={cn(blocked && "border-destructive/60")} />
          {blocked && <p className="text-xs text-destructive">Kemono and Coomer links aren't supported.</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
          <Button disabled={!url.trim() || blocked || busy} onClick={submit}>{busy && <Loader2Icon className="animate-spin" />}Add</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
