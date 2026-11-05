import { useEffect, useMemo, useRef, useState } from "react";
import { BugIcon, CircleCheckIcon, CircleXIcon, InfoIcon, SearchIcon, TriangleAlertIcon } from "lucide-react";
import { useModel } from "@/lib/engine";
import { Input, Segmented } from "@/components/ui";
import { cn } from "@/lib/utils";

type LogRow = { timestamp: string; message: string; level: string; category: string };

const LEVEL = {
  DEBUG: { icon: BugIcon, cls: "text-muted-foreground" },
  INFO: { icon: InfoIcon, cls: "text-info" },
  SUCCESS: { icon: CircleCheckIcon, cls: "text-success" },
  WARNING: { icon: TriangleAlertIcon, cls: "text-warning" },
  ERROR: { icon: CircleXIcon, cls: "text-destructive" },
} as const;

type Filter = "all" | "warn" | "error";

/** Live engine log. `compact` drops the toolbar (used inside the Downloader). */
export function LogList({ compact = false, className }: { compact?: boolean; className?: string }) {
  const { rows } = useModel<LogRow>("appBridge.logModel", { throttleMs: 150 });
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    return rows.filter((r) => {
      if (filter === "warn" && r.level !== "WARNING" && r.level !== "ERROR") return false;
      if (filter === "error" && r.level !== "ERROR") return false;
      return !q || r.message.toLowerCase().includes(q) || r.category.toLowerCase().includes(q);
    });
  }, [rows, filter, query]);

  useEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [visible]);

  const counts = useMemo(() => ({
    warn: rows.filter((r) => r.level === "WARNING" || r.level === "ERROR").length,
    error: rows.filter((r) => r.level === "ERROR").length,
  }), [rows]);

  return (
    <div className={cn("flex min-h-0 flex-col", className)}>
      {!compact && (
        <div className="flex items-center gap-2 pb-3">
          <div className="relative max-w-xs flex-1">
            <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input className="h-8 pl-8" placeholder="Search the log…" value={query} onChange={(e) => setQuery(e.target.value)} />
          </div>
          <Segmented value={filter} onChange={setFilter} options={[
            { value: "all", label: "All", count: rows.length },
            { value: "warn", label: "Warnings", count: counts.warn },
            { value: "error", label: "Errors", count: counts.error },
          ]} />
        </div>
      )}
      <div
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
        className="min-h-0 flex-1 overflow-y-auto rounded-lg border bg-background/60 font-mono text-[12px] leading-relaxed"
        data-selectable
      >
        {visible.length === 0 ? (
          <div className="p-4 font-sans text-sm text-muted-foreground">Nothing logged yet.</div>
        ) : (
          <div className="py-1.5">
            {visible.map((r, i) => {
              const lv = LEVEL[r.level as keyof typeof LEVEL] ?? LEVEL.INFO;
              const Icon = lv.icon;
              return (
                <div key={i} className="flex gap-2.5 px-3 py-[3px] hover:bg-accent/40">
                  <span className="shrink-0 text-muted-foreground/60 tabular-nums">{r.timestamp}</span>
                  <Icon className={cn("mt-[3px] size-3.5 shrink-0", lv.cls)} />
                  <span className={cn("min-w-0 break-words", r.level === "ERROR" ? "text-destructive" : r.level === "WARNING" ? "text-warning" : r.level === "DEBUG" ? "text-muted-foreground" : "text-foreground/90")}>
                    {r.message}
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
