import { useMemo, useState } from "react";
import {
  ArchiveIcon, ChevronDownIcon, ClipboardPasteIcon, FileIcon, FilmIcon, FolderOpenIcon, ImageIcon, LayoutGridIcon,
  LinkIcon, ListPlusIcon, Loader2Icon, MusicIcon, PauseIcon, PlayIcon, SlidersHorizontalIcon, SquareIcon,
  Maximize2Icon, TriangleAlertIcon, XIcon, ZapIcon, StarIcon, EllipsisIcon, FileTextIcon, CloudDownloadIcon,
} from "lucide-react";
import { call, useProp, useSetting } from "@/lib/engine";
import { clipboard, pickFile, pickFolder } from "@/lib/native";
import { openFavorites } from "@/components/favorites";
import { openCloudLinks } from "@/components/cloud-links";
import { toast } from "sonner";
import { navigate } from "@/lib/nav";
import { useElementWidth } from "@/lib/hooks";
import { Alert, Badge, Button, Card, CardAction, Dialog, DialogContent, DialogFooter, DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger, Textarea, CardContent, CardDescription, CardHeader, CardTitle, Input, Label, Progress, Select, Separator, Switch, Tip } from "@/components/ui";
import { PageHeader, Stat } from "@/components/page";
import { LogList } from "@/components/log-list";
import { cn, percent } from "@/lib/utils";

const UNSUPPORTED = /(^|\.|\/\/)(kemono|coomer)\./i;

function detectSource(url: string): { label: string; tone: "info" | "success" | "warning" | "destructive" | "secondary" } | null {
  const u = url.trim().toLowerCase();
  if (!u) return null;
  if (UNSUPPORTED.test(u)) return { label: "Not supported", tone: "destructive" };
  if (u.includes("pawchive.pw")) return { label: "Pawchive", tone: "info" };
  if (u.includes("cum.st")) return { label: "cum.st", tone: "info" };
  if (u.includes("t.me/") || u.includes("telegram")) return { label: "Telegram", tone: "info" };
  if (u.includes("bunkr")) return { label: "Bunkr", tone: "secondary" };
  if (u.includes("erome")) return { label: "Erome", tone: "secondary" };
  if (u.includes("nhentai")) return { label: "nHentai", tone: "secondary" };
  if (u.includes("gofile.io")) return { label: "GoFile", tone: "secondary" };
  if (/mega\.(nz|co\.nz|io)/.test(u)) return { label: "MEGA", tone: "secondary" };
  if (/drive\.google\.com|docs\.google\.com|drive\.usercontent\.google\.com/.test(u)) return { label: "Google Drive", tone: "secondary" };
  if (u.includes("dropbox.com")) return { label: "Dropbox", tone: "secondary" };
  if (/^https?:\/\//.test(u)) return { label: "Link", tone: "secondary" };
  return null;
}

const TYPES = [
  { value: "all", label: "Everything", icon: LayoutGridIcon },
  { value: "images", label: "Images", icon: ImageIcon },
  { value: "videos", label: "Videos", icon: FilmIcon },
  { value: "archives", label: "Archives", icon: ArchiveIcon },
  { value: "audio", label: "Audio", icon: MusicIcon },
  { value: "links", label: "Links only", icon: LinkIcon },
];

export function DownloaderView() {
  const [ref, width] = useElementWidth<HTMLDivElement>();
  const wide = width >= 960;           // room for the session + live log beside the form

  return (
    <>
      <PageHeader title="Downloader" description="Paste a creator, post or channel link to start." />
      <div ref={ref} className="flex min-h-0 flex-1">
        <div className="@container/col min-w-0 flex-1 overflow-y-auto">
          <div className="mx-auto flex max-w-5xl flex-col gap-5 px-6 py-5">
            <LinkCard />
            <OutputCard />
            <TypesCard />
            <FiltersCard />
            {!wide && (
              <>
                <SessionCard />
                <ActivityCard className="h-96" />
              </>
            )}
          </div>
        </div>
        {wide && (
          <aside className={cn("flex shrink-0 flex-col gap-4 border-l bg-sidebar/40 p-5", width >= 1500 ? "w-[500px]" : width >= 1280 ? "w-[420px]" : "w-[360px]")}>
            <SessionCard />
            <ActivityCard className="min-h-0 flex-1" />
          </aside>
        )}
      </div>
    </>
  );
}

function LinkCard() {
  const [url, setUrl] = useSetting<string>("appBridge", "currentUrl", "");
  const isDownloading = useProp<boolean>("appBridge", "isDownloading", false);
  const selecting = useProp<boolean>("appBridge", "postSelectionLoading", false);
  const isTelegram = useProp<boolean>("appBridge", "isTelegramUrl", false);
  const favoriteMode = useProp<boolean>("appBridge", "favoriteMode", false);
  const filterType = useProp<string>("appBridge", "filterType", "all");
  const linksFound = useProp<number>("appBridge", "harvestedLinksCount", 0);
  const [listOpen, setListOpen] = useState(false);
  const [listText, setListText] = useState("");
  const source = detectSource(url);
  const blocked = source?.tone === "destructive";
  const hasUrl = url.trim().length > 0;

  const primaryLabel = isDownloading && hasUrl ? "Queue at end"
    : filterType === "links" ? "Extract links"
    : favoriteMode && !hasUrl ? "Download favorites" : "Start download";

  return (
    <Card>
      <CardContent className="flex flex-col gap-4">
        <div className="relative">
          <LinkIcon className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            className={cn("h-11 pr-32 pl-9 text-[15px]", blocked && "border-destructive/60")}
            placeholder="https://pawchive.pw/patreon/user/…"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && hasUrl && !blocked) call("appBridge", "startDownload"); }}
            autoFocus
          />
          <div className="absolute top-1/2 right-2 flex -translate-y-1/2 items-center gap-1.5">
            {source && <Badge variant={source.tone}>{source.label}</Badge>}
            {hasUrl ? (
              <Tip content="Clear">
                <Button variant="ghost" size="icon-xs" onClick={() => setUrl("")}><XIcon /></Button>
              </Tip>
            ) : (
              <Tip content="Paste">
                <Button variant="ghost" size="icon-xs" onClick={async () => setUrl((await clipboard.read()).trim())}><ClipboardPasteIcon /></Button>
              </Tip>
            )}
          </div>
        </div>
        {blocked && (
          <Alert tone="destructive" icon={TriangleAlertIcon}>
            This site isn't supported. Pawchive (pawchive.pw) has the same creators and posts — open the link there instead.
          </Alert>
        )}
        <div className="flex flex-wrap items-center gap-2">
          <Button disabled={blocked || (isDownloading && !hasUrl) || (!hasUrl && !favoriteMode)}
            onClick={() => call("appBridge", "startDownload")}>
            {isDownloading && !hasUrl ? <Loader2Icon className="animate-spin" /> : <ZapIcon />}
            {isDownloading && !hasUrl ? "Downloading…" : primaryLabel}
          </Button>
          {favoriteMode && !hasUrl ? (
            <Button variant="outline" onClick={openFavorites}><StarIcon />Browse favorites…</Button>
          ) : (
            <Button variant="outline" disabled={blocked || selecting || !hasUrl}
              onClick={() => call("appBridge", "fetchPostsForSelection")}>
              {selecting ? <Loader2Icon className="animate-spin" /> : <LayoutGridIcon />}
              {selecting ? "Fetching posts…" : "Select posts…"}
            </Button>
          )}
          <Button variant="ghost" disabled={blocked || !hasUrl} onClick={() => call("appBridge", "addToQueue")}>
            <ListPlusIcon />Add to queue
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="ml-auto"><EllipsisIcon /></Button></DropdownMenuTrigger>
            <DropdownMenuContent className="w-60">
              <DropdownMenuItem onSelect={() => setListOpen(true)}><ListPlusIcon />Paste a list of links…</DropdownMenuItem>
              <DropdownMenuItem onSelect={async () => {
                const f = await pickFile("Load links from a file", [{ name: "Text", extensions: ["txt", "csv", "list"] }]);
                if (f) { const n = await call<number>("appBridge", "batchLoadUrls", f); toast.success(n ? `Queued ${n} link${n === 1 ? "" : "s"}` : "No links found in that file"); }
              }}><FileTextIcon />Load links from a file…</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={openFavorites}><StarIcon />Account favorites…</DropdownMenuItem>
              <DropdownMenuItem onSelect={openCloudLinks}><CloudDownloadIcon />Collected links…</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
        {linksFound > 0 && !isDownloading && (
          <Alert tone="info" icon={LinkIcon} action={<Button size="xs" onClick={openCloudLinks}><CloudDownloadIcon />Review</Button>}>
            {linksFound} external link{linksFound === 1 ? "" : "s"} found in posts.
          </Alert>
        )}
        <Dialog open={listOpen} onOpenChange={setListOpen}>
          <DialogContent title="Paste a list of links" description="One link per line. Each one is queued with the current options.">
            <Textarea autoFocus className="min-h-40 font-mono text-[13px]" value={listText} onChange={(e) => setListText(e.target.value)} placeholder={"https://pawchive.pw/patreon/user/…\nhttps://cum.st/onlyfans/user/…"} />
            <DialogFooter>
              <Button variant="outline" onClick={() => setListOpen(false)}>Cancel</Button>
              <Button disabled={!listText.trim()} onClick={async () => {
                const n = await call<number>("appBridge", "batchLoadUrls", listText);
                toast.success(n ? `Queued ${n} link${n === 1 ? "" : "s"}` : "No links found");
                setListText(""); setListOpen(false);
              }}><ListPlusIcon />Queue links</Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
        {isTelegram && <p className="-mt-1 text-xs text-muted-foreground">Telegram: you'll choose how much of the channel to get.</p>}
      </CardContent>
    </Card>
  );
}

function TypesCard() {
  const [filterType, setFilterType] = useSetting<string>("appBridge", "filterType", "all");
  return (
    <Card>
      <CardHeader>
        <CardTitle>What to download</CardTitle>
        <CardDescription>Pick the kinds of files to keep from each post.</CardDescription>
      </CardHeader>
      <CardContent className="grid grid-cols-3 gap-2 @xl/col:grid-cols-6">
        {TYPES.map((t) => {
          const Icon = t.icon;
          const active = filterType === t.value;
          return (
            <button key={t.value} onClick={() => setFilterType(t.value)}
              className={cn(
                "flex min-w-0 flex-col items-center gap-2 rounded-lg border px-2 py-3 text-xs font-medium transition-colors",
                active ? "border-primary/60 bg-primary/5 text-foreground ring-1 ring-primary/30" : "text-muted-foreground hover:bg-accent/50 hover:text-foreground",
              )}>
              <Icon className="size-[18px]" /><span className="max-w-full truncate">{t.label}</span>
            </button>
          );
        })}
      </CardContent>
    </Card>
  );
}

function ActivityCard({ className }: { className?: string }) {
  return (
    <Card className={cn("gap-3 pt-4 pb-3", className)}>
      <CardHeader className="px-4">
        <CardTitle className="text-sm">Activity</CardTitle>
        <CardAction>
          <Tip content="Open the full log">
            <Button variant="ghost" size="icon-xs" className="-mt-1" onClick={() => navigate("activity")}><Maximize2Icon /></Button>
          </Tip>
        </CardAction>
      </CardHeader>
      <CardContent className="flex min-h-0 flex-1 flex-col px-3"><LogList compact className="min-h-0 flex-1" /></CardContent>
    </Card>
  );
}

function OutputCard() {
  const [dir, setDir] = useSetting<string>("appBridge", "downloadDir", "");
  const [pageStart, setPageStart] = useSetting<number>("appBridge", "pageStart", 1);
  const [pageEnd, setPageEnd] = useSetting<number>("appBridge", "pageEnd", 999999);
  const allPages = pageEnd >= 999999;
  return (
    <Card>
      <CardContent className="grid gap-5 @3xl/col:grid-cols-[minmax(0,1fr)_auto]">
        <div className="flex min-w-0 flex-col gap-2">
          <Label>Save to</Label>
          <div className="flex gap-2">
            <Input value={dir} onChange={(e) => setDir(e.target.value)} className="font-mono text-[13px]" />
            <Button variant="outline" onClick={async () => { const d = await pickFolder("Download folder", dir); if (d) setDir(d); }}>
              <FolderOpenIcon />Browse
            </Button>
            <Button variant="ghost" size="icon" title="Open folder" onClick={() => call("appBridge", "openDownloadFolder")}><FileIcon /></Button>
          </div>
        </div>
        <div className="flex flex-col gap-2">
          <Label>Pages</Label>
          <div className="flex items-center gap-2">
            <Input type="number" min={1} className="w-20" value={pageStart} onChange={(e) => setPageStart(Math.max(1, Number(e.target.value) || 1))} />
            <span className="text-sm text-muted-foreground">to</span>
            <Input className="w-20" placeholder="All" value={allPages ? "" : pageEnd}
              onChange={(e) => setPageEnd(e.target.value.trim() ? Math.max(1, Number(e.target.value) || 1) : 999999)} />
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

function FiltersCard() {
  const [open, setOpen] = useState(false);
  const [skipWords, setSkipWords] = useSetting<string>("appBridge", "skipWords", "");
  const [characters, setCharacters] = useSetting<string>("appBridge", "filterCharacters", "");
  const [removeWords, setRemoveWords] = useSetting<string>("appBridge", "removeWords", "");
  const [dateAfter, setDateAfter] = useSetting<string>("appBridge", "dateAfter", "");
  const [dateBefore, setDateBefore] = useSetting<string>("appBridge", "dateBefore", "");
  const [minSize, setMinSize] = useSetting<string>("appBridge", "minFileSize", "");
  const [maxSize, setMaxSize] = useSetting<string>("appBridge", "maxFileSize", "");
  const [profile] = useSetting<string>("appBridge", "filterProfile", "clear");
  const [rwProfile] = useSetting<string>("appBridge", "removeWordsProfile", "clear");

  const active = useMemo(() => [skipWords, characters, removeWords, dateAfter, dateBefore, minSize, maxSize].filter((v) => v && String(v).trim()).length,
    [skipWords, characters, removeWords, dateAfter, dateBefore, minSize, maxSize]);

  return (
    <Card className="gap-0 py-0">
      <button className="flex items-center gap-3 px-5 py-4 text-left" onClick={() => setOpen(!open)}>
        <SlidersHorizontalIcon className="size-4 text-muted-foreground" />
        <div className="flex-1">
          <div className="font-semibold">Filters & options</div>
          <div className="text-sm text-muted-foreground">Skip words, characters, dates, sizes and file handling.</div>
        </div>
        {active > 0 && <Badge variant="info">{active} active</Badge>}
        <ChevronDownIcon className={cn("size-4 text-muted-foreground transition-transform", open && "rotate-180")} />
      </button>
      {open && (
        <>
          <Separator />
          <div className="grid gap-x-6 gap-y-5 p-5 @2xl/col:grid-cols-2">
            <div className="flex flex-col gap-2">
              <div className="flex items-center justify-between"><Label>Skip posts containing</Label>
                <Select size="sm" value={profile} onValueChange={(v) => call("appBridge", "applyFilterProfile", v)} options={[
                  { value: "clear", label: "No preset" }, { value: "wip_sketches", label: "WIP & sketches" },
                  { value: "low_quality", label: "Low quality & previews" }, { value: "wip_and_low_quality", label: "WIP, sketches & low-res" },
                  { value: "watermarks_samples", label: "Watermarks & samples" }, { value: "final_art_only", label: "Final art only" },
                  { value: "large_media_only", label: "Large media only" }, { value: "custom", label: "Custom" },
                ]} />
              </div>
              <Input placeholder="wip, sketch, preview" value={skipWords} onChange={(e) => setSkipWords(e.target.value)} />
            </div>
            <div className="flex flex-col gap-2">
              <Label>Only characters</Label>
              <Input placeholder="Comma-separated names" value={characters} onChange={(e) => setCharacters(e.target.value)} />
            </div>
            <div className="flex flex-col gap-2">
              <div className="flex items-center justify-between"><Label>Remove from file names</Label>
                <Select size="sm" value={rwProfile} onValueChange={(v) => call("appBridge", "applyRemoveWordsProfile", v)} options={[
                  { value: "clear", label: "No preset" }, { value: "clean_names", label: "Clean names" },
                  { value: "patreon_promo", label: "Patreon & promo" }, { value: "watermark_tags", label: "Watermark tags" }, { value: "custom", label: "Custom" },
                ]} />
              </div>
              <Input placeholder="patreon, HD, [sample]" value={removeWords} onChange={(e) => setRemoveWords(e.target.value)} />
            </div>
            <div className="flex flex-col gap-2">
              <Label>Posted between</Label>
              <div className="flex items-center gap-2">
                <Input type="date" value={dateAfter} onChange={(e) => setDateAfter(e.target.value)} />
                <span className="text-sm text-muted-foreground">–</span>
                <Input type="date" value={dateBefore} onChange={(e) => setDateBefore(e.target.value)} />
              </div>
            </div>
            <div className="flex flex-col gap-2">
              <Label>File size</Label>
              <div className="flex items-center gap-2">
                <Input placeholder="Min, e.g. 500 KB" value={minSize} onChange={(e) => setMinSize(e.target.value)} />
                <span className="text-sm text-muted-foreground">–</span>
                <Input placeholder="Max, e.g. 2 GB" value={maxSize} onChange={(e) => setMaxSize(e.target.value)} />
              </div>
            </div>
          </div>
          <Separator />
          <div className="grid gap-x-8 p-5 pt-2 @2xl/col:grid-cols-2">
            <Toggle prop="subfolderPerPost" label="Folder per post" />
            <Toggle prop="skipArchives" label="Skip archives" />
            <Toggle prop="scanContentImages" label="Images embedded in post text" />
            <Toggle prop="downloadRevisions" label="Include post revisions" />
            <Toggle prop="skipPostCovers" label="Skip post covers" />
            <Toggle prop="downloadThumbnailsOnly" label="Thumbnails only" />
            <Toggle prop="fallbackToThumbnails" label="Fall back to thumbnails" />
            <Toggle prop="keepDuplicates" label="Keep duplicate files" />
            <Toggle prop="compressWebp" label="Convert images to WebP" />
            <Toggle prop="downloadEmbeds" label="Download embedded videos" />
            <Toggle prop="favoriteMode" label="Favorites mode" />
            <Toggle prop="downloadDms" label="Include DMs" />
          </div>
        </>
      )}
    </Card>
  );
}

export function Toggle({ prop, label, obj = "appBridge", hint }: { prop: string; label: string; obj?: string; hint?: string }) {
  const [v, set] = useSetting<boolean>(obj, prop, false);
  return (
    <label className="flex cursor-default items-center justify-between gap-4 py-2.5">
      <span className="min-w-0">
        <span className="block text-sm">{label}</span>
        {hint && <span className="block text-[13px] text-muted-foreground">{hint}</span>}
      </span>
      <Switch checked={!!v} onCheckedChange={set} />
    </label>
  );
}

function SessionCard() {
  const downloading = useProp<boolean>("appBridge", "isDownloading", false);
  const paused = useProp<boolean>("appBridge", "isPaused", false);
  const status = useProp<string>("appBridge", "statusText", "Idle");
  const creator = useProp<string>("appBridge", "creatorName", "");
  const progress = percent(useProp<number>("appBridge", "overallProgress", 0));
  const speed = useProp<string>("appBridge", "speedText", "—");
  const eta = useProp<string>("appBridge", "etaText", "—");
  const elapsed = useProp<string>("appBridge", "elapsedTimeText", "—");
  const files = useProp<string>("appBridge", "filesCountText", "—");
  const saved = useProp<string>("appBridge", "savedBytesText", "—");
  const threads = useProp<number>("appBridge", "threadsCount", 0);
  const autoRetry = useProp<boolean>("appBridge", "autoRetryAtEnd", false);

  return (
    <Card>
      <CardHeader>
        <CardTitle>Session</CardTitle>
        <CardDescription className="truncate">{creator || status || "Idle"}</CardDescription>
        <CardAction>
          {downloading ? (paused ? <Badge variant="warning">Paused</Badge> : <Badge variant="success">Downloading</Badge>) : <Badge variant="outline">Idle</Badge>}
        </CardAction>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <div className="flex items-end justify-between">
          <span className="text-3xl font-semibold tracking-tight tabular-nums">{progress.toFixed(0)}<span className="text-lg text-muted-foreground">%</span></span>
          <span className="pb-1 text-sm text-muted-foreground tabular-nums">{downloading ? speed : ""}</span>
        </div>
        <Progress value={progress} className="h-2" />
        <div className="grid grid-cols-3 gap-3">
          <Stat label="Files" value={files || "—"} />
          <Stat label="Saved" value={saved || "—"} />
          <Stat label="Threads" value={threads || "—"} />
          <Stat label="Elapsed" value={elapsed || "—"} />
          <Stat label="Remaining" value={downloading ? eta || "—" : "—"} />
          <Stat label="Speed" value={downloading ? speed || "—" : "—"} />
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" className="flex-1" disabled={!downloading}
            onClick={() => call("appBridge", paused ? "resumeDownload" : "pauseDownload")}>
            {paused ? <PlayIcon /> : <PauseIcon />}{paused ? "Resume" : "Pause"}
          </Button>
          <Button variant="outline" size="sm" className="flex-1" disabled={!downloading} onClick={() => call("appBridge", "cancelDownload")}>
            <SquareIcon />Stop
          </Button>
        </div>
        <label className="flex items-center justify-between text-sm">
          <span className="text-muted-foreground">Retry failed files at the end</span>
          <Switch checked={autoRetry} onCheckedChange={() => call("appBridge", "toggleAutoRetry")} />
        </label>
      </CardContent>
    </Card>
  );
}
