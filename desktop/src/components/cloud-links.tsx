import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { CloudDownloadIcon, CopyIcon, FileDownIcon, FolderOpenIcon, LinkIcon } from "lucide-react";
import { toast } from "sonner";
import { call, engine, useProp } from "@/lib/engine";
import { clipboard, pickFolder } from "@/lib/native";
import { Badge, Button, Dialog, DialogContent, DialogFooter, Empty, Input, Label } from "@/components/ui";
import { cn } from "@/lib/utils";

type Harvested = { title: string; url: string; platform: string; creator: string; post_id: string };

let isOpen = false;
const listeners = new Set<() => void>();
export function openCloudLinks() { isOpen = true; listeners.forEach((l) => l()); }
const close = () => { isOpen = false; listeners.forEach((l) => l()); };
const useOpen = () => useSyncExternalStore((l) => { listeners.add(l); return () => { listeners.delete(l); }; }, () => isOpen);

const PRETTY: Record<string, string> = { mega: "MEGA", google_drive: "Google Drive", gdrive: "Google Drive", dropbox: "Dropbox", gofile: "GoFile", mediafire: "MediaFire", pixeldrain: "Pixeldrain" };
const pretty = (p: string) => PRETTY[p?.toLowerCase()] ?? (p ? p.charAt(0).toUpperCase() + p.slice(1) : "Other");

/** Links collected by a "Links only" run: review and download them. */
export function CloudLinksDialog() {
  const open = useOpen();
  const links = useProp<Harvested[]>("appBridge", "harvestedLinks", []) ?? [];
  const count = useProp<number>("appBridge", "harvestedLinksCount", 0);
  // The list can be long, so the engine doesn't push it: fetch it when it's needed
  useEffect(() => { if (open) engine.fetch("appBridge", "harvestedLinks").catch(() => {}); }, [open, count]);
  const busy = useProp<boolean>("appBridge", "isCloudDownloading", false);
  const downloadDir = useProp<string>("appBridge", "downloadDir", "");
  const [picked, setPicked] = useState<Set<string> | null>(null);
  const [dest, setDest] = useState("");

  const all = useMemo(() => [...new Map(links.map((l) => [l.url, l])).values()], [links]);
  const selected = picked ?? new Set(all.map((l) => l.url));
  const byPlatform = useMemo(() => {
    const m = new Map<string, Harvested[]>();
    for (const l of all) { const k = l.platform || "other"; if (!m.has(k)) m.set(k, []); m.get(k)!.push(l); }
    return [...m.entries()];
  }, [all]);
  const toggle = (u: string) => { const n = new Set(selected); if (n.has(u)) n.delete(u); else n.add(u); setPicked(n); };

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) { close(); setPicked(null); } }}>
      <DialogContent className="flex h-[78vh] max-w-3xl flex-col" title="Collected links" description={`${all.length} external links found in posts. ${selected.size} selected.`}>
        {all.length === 0 ? (
          <Empty icon={LinkIcon} title="No links collected" description='Pick "Links only" in the Downloader to scan posts for MEGA, Google Drive, Dropbox and GoFile links.' />
        ) : (
          <div className="-mx-1 min-h-0 flex-1 overflow-y-auto px-1">
            {byPlatform.map(([platform, items]) => (
              <section key={platform} className="mb-4">
                <div className="mb-1.5 flex items-center gap-2 text-xs font-medium text-muted-foreground">
                  {pretty(platform)} <span className="tabular-nums">{items.length}</span>
                </div>
                <div className="overflow-hidden rounded-lg border">
                  {items.map((l) => (
                    <label key={l.url} className={cn("flex cursor-default items-center gap-3 border-b px-3 py-2 last:border-b-0 hover:bg-accent/30", !selected.has(l.url) && "opacity-60")}>
                      <input type="checkbox" className="size-4 accent-current" checked={selected.has(l.url)} onChange={() => toggle(l.url)} />
                      <div className="min-w-0 flex-1">
                        <div className="truncate font-mono text-[12px]" data-selectable>{l.url}</div>
                        <div className="truncate text-xs text-muted-foreground">{l.creator}{l.title ? ` · ${l.title}` : ""}</div>
                      </div>
                      <Badge variant="outline">{pretty(l.platform)}</Badge>
                    </label>
                  ))}
                </div>
              </section>
            ))}
          </div>
        )}
        <div className="flex flex-col gap-2">
          <Label>Save to</Label>
          <div className="flex gap-2">
            <Input value={dest} placeholder={downloadDir} onChange={(e) => setDest(e.target.value)} className="font-mono text-[13px]" />
            <Button variant="outline" onClick={async () => { const d = await pickFolder("Save cloud downloads to", dest || downloadDir); if (d) setDest(d); }}><FolderOpenIcon />Browse</Button>
          </div>
        </div>
        <DialogFooter>
          <Button variant="ghost" className="mr-auto" disabled={!all.length} onClick={() => { clipboard.write([...selected].join("\n")); toast.success("Links copied"); }}><CopyIcon />Copy</Button>
          <Button variant="outline" disabled={!all.length} onClick={() => call("appBridge", "exportAllLinks")}><FileDownIcon />Export…</Button>
          <Button disabled={!selected.size || busy} onClick={() => { call("appBridge", "startCloudDownloads", [...selected], dest); toast.success(`Downloading ${selected.size} link${selected.size === 1 ? "" : "s"}`); close(); }}>
            <CloudDownloadIcon />Download {selected.size || ""}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
