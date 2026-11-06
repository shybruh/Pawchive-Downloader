import type { ComponentType } from "react";
import {
  ArchiveIcon, BookmarkIcon, CalendarClockIcon, DownloadIcon, FileArchiveIcon, HistoryIcon, ImagesIcon,
  LinkIcon, ListOrderedIcon, ScrollTextIcon, SettingsIcon, TagsIcon,
} from "lucide-react";
import { useProp } from "@/lib/engine";
import { navigate, useNav, type ViewId } from "@/lib/nav";
import { useUpdater } from "@/lib/updater";
import { cn } from "@/lib/utils";
import logo from "@/assets/logo.png";


type Item = { id: ViewId; label: string; icon: ComponentType<{ className?: string }>; badge?: number | string };

function NavItem({ item, active, onClick }: { item: Item; active: boolean; onClick: () => void }) {
  const Icon = item.icon;
  return (
    <button
      onClick={onClick}
      className={cn(
        "group flex h-8 w-full items-center gap-2.5 rounded-md px-2 text-sm transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
        active ? "bg-accent font-medium text-accent-foreground" : "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
      )}
    >
      <Icon className={cn("size-4 shrink-0", active ? "text-foreground" : "text-muted-foreground group-hover:text-foreground")} />
      <span className="truncate">{item.label}</span>
      {item.badge ? (
        <span className="ml-auto rounded-md bg-secondary px-1.5 py-px text-[11px] font-medium tabular-nums text-secondary-foreground">{item.badge}</span>
      ) : null}
    </button>
  );
}

function Group({ label, items, view, onNavigate }: { label: string; items: Item[]; view: ViewId; onNavigate: (v: ViewId) => void }) {
  return (
    <div className="flex flex-col gap-0.5">
      <div className="px-2 pb-1 text-xs font-medium text-muted-foreground/70">{label}</div>
      {items.map((i) => <NavItem key={i.id} item={i} active={view === i.id} onClick={() => onNavigate(i.id)} />)}
    </div>
  );
}

export function AppSidebar() {
  const { view } = useNav();
  const onNavigate = (v: ViewId) => navigate(v);
  const queueActive = useProp<number>("appBridge.queueModel", "downloadingCount", 0);
  const queuePending = useProp<number>("appBridge.queueModel", "pendingCount", 0);
  const newPosts = useProp<number>("appBridge.watchlistModel", "totalNewPosts", 0);
  const { phase } = useUpdater();
  const updateAvailable = phase === "available";
  const queued = queueActive + queuePending;

  return (
    <aside className="flex w-60 shrink-0 flex-col border-r border-sidebar-border bg-sidebar">
      <div className="flex h-14 items-center gap-2.5 px-4">
        <img src={logo} alt="" className="size-7 rounded-md" />
        <span className="text-[15px] font-semibold tracking-tight">Pawchive</span>
      </div>

      <nav className="flex flex-1 flex-col gap-5 overflow-y-auto px-3 py-2">
        <Group label="Download" view={view} onNavigate={onNavigate} items={[
          { id: "downloader", label: "Downloader", icon: DownloadIcon },
          { id: "queue", label: "Queue", icon: ListOrderedIcon, badge: queued || undefined },
          { id: "watchlist", label: "Watchlist", icon: BookmarkIcon, badge: newPosts ? `+${newPosts}` : undefined },
          { id: "activity", label: "Activity log", icon: ScrollTextIcon },
        ]} />
        <Group label="Library" view={view} onNavigate={onNavigate} items={[
          { id: "gallery", label: "Gallery", icon: ImagesIcon },
          { id: "history", label: "History", icon: HistoryIcon },
          { id: "archive", label: "Archive", icon: ArchiveIcon },
          { id: "known", label: "Known series", icon: TagsIcon },
          { id: "vault", label: "Link vault", icon: LinkIcon },
        ]} />
        <Group label="Tools" view={view} onNavigate={onNavigate} items={[
          { id: "decompressor", label: "Decompressor", icon: FileArchiveIcon },
          { id: "scheduler", label: "Scheduler", icon: CalendarClockIcon },
        ]} />
      </nav>

      <div className="border-t border-sidebar-border p-3">
        <NavItem item={{ id: "settings", label: "Settings", icon: SettingsIcon, badge: updateAvailable ? "Update" : undefined }}
          active={view === "settings"} onClick={() => (updateAvailable ? navigate("settings", { section: "updates" }) : onNavigate("settings"))} />
      </div>
    </aside>
  );
}
