import { useEffect, useMemo, useRef, useState } from "react";
import {
  CircleIcon, ClipboardCopyIcon, CopyIcon, EllipsisIcon, ExternalLinkIcon, FileArchiveIcon, FolderOutputIcon, KeyRoundIcon,
  LinkIcon, Loader2Icon, PencilIcon, PickaxeIcon, SearchIcon, ShieldCheckIcon, TrashIcon, UserRoundIcon, XIcon,
} from "lucide-react";
import { toast } from "sonner";
import { call, callAsync, useModel, useProp, useSignal } from "@/lib/engine";
import { clipboard, openExternal, pickFolder } from "@/lib/native";
import {
  Alert, Badge, Button, Dialog, DialogContent, DialogFooter, DropdownMenu, DropdownMenuContent, DropdownMenuItem,
  DropdownMenuSeparator, DropdownMenuTrigger, Empty, Input, Label, Select, Switch, Textarea, Tip,
} from "@/components/ui";
import { PageHeader } from "@/components/page";
import { cn } from "@/lib/utils";

type VaultLink = { id: string; url: string; platform: string; passwords: string[]; health: "alive" | "dead" | "unknown"; last_checked: string | null };
type VaultPost = { post_id: string; title: string; published: string; full_text: string; post_url: string; passwords: string[]; links_count: number; links: VaultLink[] };
type VaultCreator = { creator_key: string; creator_name: string; service: string; user_id: string; post_count: number; link_count: number; updated_at: string; posts: VaultPost[] };

const PRETTY: Record<string, string> = {
  mega: "MEGA", google_drive: "Google Drive", gdrive: "Google Drive", dropbox: "Dropbox", mediafire: "MediaFire",
  pixeldrain: "Pixeldrain", gofile: "Gofile", onedrive: "OneDrive", catbox: "Catbox", other: "Other",
};
const prettyPlatform = (p: string) => PRETTY[p?.toLowerCase()] ?? (p ? p.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase()) : "Other");

function HealthDot({ health }: { health: VaultLink["health"] }) {
  const cls = health === "alive" ? "fill-success text-success" : health === "dead" ? "fill-destructive text-destructive" : "fill-muted-foreground/40 text-muted-foreground/40";
  const label = health === "alive" ? "Link works" : health === "dead" ? "Link is dead" : "Not checked yet";
  return <Tip content={label}><CircleIcon className={cn("size-2 shrink-0", cls)} /></Tip>;
}

export function VaultView() {
  const totalLinks = useProp<number>("appBridge", "linkVaultTotalLinks", 0);
  const totalCreators = useProp<number>("appBridge", "linkVaultTotalCreators", 0);
  const probing = useProp<boolean>("appBridge", "linkVaultProbingActive", false);
  const harvesting = useProp<boolean>("appBridge", "linkVaultHarvestingActive", false);
  const harvestStatus = useProp<string>("appBridge", "linkVaultHarvestingStatus", "");
  const [search, setSearch] = useState("");
  const [platform, setPlatform] = useState("all");
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<string>("");
  const [collectOpen, setCollectOpen] = useState(false);
  const { rows, refresh } = useModel<{ creator: VaultCreator }>("appBridge.linkVaultCreatorsModel", { throttleMs: 100 });
  const creators = useMemo(() => rows.map((r) => r.creator).filter(Boolean), [rows]);
  const platforms = useRef(new Set<string>());

  // Load the tree in the background whenever the vault or the filters change
  useEffect(() => {
    let cancelled = false;
    const t = setTimeout(async () => {
      setLoading(true);
      await call("appBridge", "refreshLinkVault", search, platform === "all" ? "" : platform);
      await callAsync("appBridge", "linkVaultTreeJsonAsync");
      if (cancelled) return;
      await call("appBridge.linkVaultCreatorsModel", "applyPending");
      await refresh();
      setLoading(false);
    }, 250);
    return () => { cancelled = true; clearTimeout(t); };
  }, [search, platform, totalLinks, totalCreators, probing, refresh]);

  useSignal("appBridge", "vaultHarvestFinished", (ok: boolean, name: string, newLinks: number) => {
    if (ok) toast.success(`Collected ${newLinks} new link${newLinks === 1 ? "" : "s"}${name ? ` from ${name}` : ""}`);
    else toast.error("Link collection stopped");
  });
  useSignal("appBridge", "linkVaultProbingFinished", () => toast.success("Link check finished"));

  for (const c of creators) for (const p of c.posts ?? []) for (const l of p.links ?? []) platforms.current.add((l.platform || "other").toLowerCase());
  const current = creators.find((c) => c.creator_key === selected) ?? creators[0];

  return (
    <>
      <PageHeader title="Link vault" description={`${totalLinks} links from ${totalCreators} creators`}
        actions={<>
          <Button variant="outline" size="sm" disabled={probing || !totalLinks} onClick={() => call("appBridge", "probeVaultHealth")}>
            {probing ? <Loader2Icon className="animate-spin" /> : <ShieldCheckIcon />}{probing ? "Checking…" : "Check links"}
          </Button>
          <Button size="sm" disabled={harvesting} onClick={() => setCollectOpen(true)}><PickaxeIcon />Collect links</Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm"><EllipsisIcon /></Button></DropdownMenuTrigger>
            <DropdownMenuContent className="w-60">
              <DropdownMenuItem disabled={!totalLinks} onSelect={async () => { const n = await call<number>("appBridge", "copyVaultLinks", ""); toast.success(`Copied ${n} links`); }}>
                <ClipboardCopyIcon />Copy every link
              </DropdownMenuItem>
              <DropdownMenuItem disabled={!totalLinks} onSelect={async () => {
                const dir = await pickFolder("Export the vault to…"); if (!dir) return;
                const n = await call<number>("appBridge", "exportVaultToFolder", dir); toast.success(`Exported ${n} links`);
              }}><FolderOutputIcon />Export to a folder…</DropdownMenuItem>
              <DropdownMenuItem onSelect={async () => { const n = await call<number>("appBridge", "copyPasswordsToDecompressor"); toast.success(`${n} passwords sent to the Decompressor`); }}>
                <FileArchiveIcon />Send passwords to Decompressor
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onSelect={async () => { const n = await call<number>("appBridge", "cleanDeadVaultLinks"); toast.success(`Removed ${n} dead links`); }}>
                <TrashIcon />Remove dead links
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </>}>
        <div className="relative w-full max-w-80 min-w-44 flex-1">
          <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input className="h-8 pl-8" placeholder="Search creators, posts, links or passwords…" value={search} onChange={(e) => setSearch(e.target.value)} />
        </div>
        <Select size="sm" value={platform} onValueChange={setPlatform} className="w-44"
          options={[{ value: "all", label: "All hosts" }, ...[...platforms.current].sort().map((p) => ({ value: p, label: prettyPlatform(p) }))]} />
        {loading && <Loader2Icon className="size-4 animate-spin text-muted-foreground" />}
      </PageHeader>

      {harvesting && (
        <Alert className="mx-6 mt-4" tone="info" icon={Loader2Icon} busy
          action={<Button size="xs" variant="outline" onClick={() => call("appBridge", "cancelVaultHarvest")}><XIcon />Stop</Button>}>
          <span className="block truncate">{harvestStatus || "Collecting links…"}</span>
        </Alert>
      )}

      {creators.length === 0 ? (
        <Empty icon={LinkIcon} title={totalLinks ? "Nothing matches" : "The vault is empty"}
          description={totalLinks ? "Try another search or host." : "External links (MEGA, Google Drive, Dropbox…) found in posts are kept here, with their passwords."}>
          {!totalLinks && <Button size="sm" onClick={() => setCollectOpen(true)}><PickaxeIcon />Collect links from a creator</Button>}
        </Empty>
      ) : (
        <div className="flex min-h-0 flex-1">
          <div className="w-64 shrink-0 overflow-y-auto border-r p-2">
            {creators.map((c) => (
              <button key={c.creator_key} onClick={() => setSelected(c.creator_key)}
                className={cn("flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-left",
                  current?.creator_key === c.creator_key ? "bg-accent" : "hover:bg-accent/50")}>
                <UserRoundIcon className="size-4 shrink-0 text-muted-foreground" />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium">{c.creator_name}</div>
                  <div className="truncate text-xs text-muted-foreground capitalize">{c.service}</div>
                </div>
                <span className="text-xs text-muted-foreground tabular-nums">{c.link_count}</span>
              </button>
            ))}
          </div>
          {current && <CreatorDetail creator={current} />}
        </div>
      )}
      <CollectDialog open={collectOpen} onOpenChange={setCollectOpen} />
    </>
  );
}

function CreatorDetail({ creator }: { creator: VaultCreator }) {
  const [editing, setEditing] = useState<VaultLink | null>(null);
  return (
    <div className="min-w-0 flex-1 overflow-y-auto">
      <div className="sticky top-0 z-10 flex items-center gap-3 border-b bg-background/95 px-6 py-3 backdrop-blur">
        <div className="min-w-0 flex-1">
          <div className="truncate font-medium">{creator.creator_name}</div>
          <div className="text-xs text-muted-foreground">{creator.link_count} links in {creator.post_count} posts</div>
        </div>
        <Button variant="outline" size="sm" onClick={async () => { const n = await call<number>("appBridge", "copyVaultLinks", creator.creator_key); toast.success(`Copied ${n} links`); }}>
          <CopyIcon />Copy links
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm"><EllipsisIcon /></Button></DropdownMenuTrigger>
          <DropdownMenuContent>
            <DropdownMenuItem variant="destructive" onSelect={() => call("appBridge", "deleteVaultCreator", creator.creator_key)}><TrashIcon />Remove creator</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <div className="flex flex-col gap-4 px-6 py-5">
        {creator.posts.map((p) => (
          <div key={p.post_id} className="rounded-xl border bg-card">
            <div className="flex items-start gap-3 border-b px-4 py-3">
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium">{p.title || "Untitled post"}</div>
                <div className="text-xs text-muted-foreground">{String(p.published || "").slice(0, 10)}{p.passwords?.length ? ` · ${p.passwords.length} password${p.passwords.length === 1 ? "" : "s"}` : ""}</div>
              </div>
              {p.post_url && (
                <Tip content="Open the post"><Button variant="ghost" size="icon-xs" onClick={() => openExternal(p.post_url)}><ExternalLinkIcon /></Button></Tip>
              )}
              <DropdownMenu>
                <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-xs"><EllipsisIcon /></Button></DropdownMenuTrigger>
                <DropdownMenuContent>
                  <DropdownMenuItem variant="destructive" onSelect={() => call("appBridge", "deleteVaultPost", p.post_id)}><TrashIcon />Remove post</DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
            <div className="divide-y">
              {p.links.map((l) => (
                <div key={l.id} className="group flex items-center gap-3 px-4 py-2.5">
                  <HealthDot health={l.health} />
                  <Badge variant="outline" className="w-24 justify-center">{prettyPlatform(l.platform)}</Badge>
                  <div className="min-w-0 flex-1">
                    <div className={cn("truncate font-mono text-[12px]", l.health === "dead" && "text-muted-foreground line-through")} data-selectable>{l.url}</div>
                    {l.passwords?.length > 0 && (
                      <div className="mt-1 flex flex-wrap gap-1">
                        {l.passwords.map((pw) => (
                          <button key={pw} onClick={() => { clipboard.write(pw); toast.success("Password copied"); }}
                            className="inline-flex items-center gap-1 rounded border bg-muted/50 px-1.5 py-px font-mono text-[11px] hover:bg-accent">
                            <KeyRoundIcon className="size-3 text-muted-foreground" />{pw}
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                  <div className="flex shrink-0 gap-0.5 opacity-60 group-hover:opacity-100">
                    <Tip content="Copy link"><Button variant="ghost" size="icon-xs" onClick={() => { clipboard.write(l.url); toast.success("Link copied"); }}><CopyIcon /></Button></Tip>
                    <Tip content="Open"><Button variant="ghost" size="icon-xs" onClick={() => openExternal(l.url)}><ExternalLinkIcon /></Button></Tip>
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-xs"><EllipsisIcon /></Button></DropdownMenuTrigger>
                      <DropdownMenuContent>
                        <DropdownMenuItem onSelect={() => setEditing(l)}><PencilIcon />Edit passwords</DropdownMenuItem>
                        <DropdownMenuSeparator />
                        <DropdownMenuItem variant="destructive" onSelect={() => call("appBridge", "deleteVaultLink", l.id)}><TrashIcon />Remove link</DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </div>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
      <PasswordsDialog link={editing} onClose={() => setEditing(null)} />
    </div>
  );
}

function PasswordsDialog({ link, onClose }: { link: VaultLink | null; onClose: () => void }) {
  const [text, setText] = useState("");
  useEffect(() => { setText((link?.passwords ?? []).join("\n")); }, [link]);
  return (
    <Dialog open={!!link} onOpenChange={(o) => !o && onClose()}>
      <DialogContent title="Passwords" description="One per line. Used when extracting this link's archives.">
        <Textarea className="min-h-32 font-mono text-[13px]" value={text} onChange={(e) => setText(e.target.value)} />
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>Cancel</Button>
          <Button onClick={async () => {
            if (!link) return;
            await call("appBridge", "updateVaultLinkPasswords", link.id, text.split("\n").map((s) => s.trim()).filter(Boolean));
            toast.success("Passwords saved");
            onClose();
          }}>Save</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function CollectDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const [url, setUrl] = useState("");
  const [probe, setProbe] = useState(true);
  const [from, setFrom] = useState(1);
  const [to, setTo] = useState("");
  const blocked = /(kemono|coomer)\./i.test(url);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent title="Collect links" description="Scans a creator's posts for external links and their passwords. Nothing is downloaded.">
        <div className="flex flex-col gap-4">
          <div className="flex flex-col gap-2">
            <Label>Creator link</Label>
            <Input autoFocus placeholder="https://pawchive.pw/patreon/user/…" value={url} onChange={(e) => setUrl(e.target.value)} className={cn(blocked && "border-destructive/60")} />
            {blocked && <p className="text-xs text-destructive">Kemono and Coomer links aren't supported.</p>}
          </div>
          <div className="flex items-end gap-4">
            <div className="flex flex-col gap-2">
              <Label>Pages</Label>
              <div className="flex items-center gap-2">
                <Input type="number" min={1} className="w-20" value={from} onChange={(e) => setFrom(Math.max(1, Number(e.target.value) || 1))} />
                <span className="text-sm text-muted-foreground">to</span>
                <Input className="w-20" placeholder="All" value={to} onChange={(e) => setTo(e.target.value.replace(/\D/g, ""))} />
              </div>
            </div>
            <label className="ml-auto flex items-center gap-2 pb-2 text-sm">Check links afterwards <Switch checked={probe} onCheckedChange={setProbe} /></label>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
          <Button disabled={!url.trim() || blocked} onClick={() => {
            call("appBridge", "harvestArtistToVault", url.trim(), probe, from, to ? Number(to) : 999999);
            onOpenChange(false);
          }}><PickaxeIcon />Collect</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
