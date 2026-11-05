import { useMemo, useState } from "react";
import { HistoryIcon, RotateCwIcon, SearchIcon } from "lucide-react";
import { call, setProp, useQuery, useSignal } from "@/lib/engine";
import { Badge, Button, Empty, Input, Tip } from "@/components/ui";
import { PageBody, PageHeader } from "@/components/page";

type Session = { creator: string; url: string; service: string; files: number; date: string };

export function HistoryView() {
  const { data, reload } = useQuery<Session[]>("appBridge", "getDownloadHistory");
  useSignal("appBridge", "downloadHistoryChanged", () => reload());
  const [q, setQ] = useState("");
  const list = useMemo(() => {
    const s = q.trim().toLowerCase();
    return (data ?? []).filter((h) => !s || `${h.creator} ${h.url} ${h.service}`.toLowerCase().includes(s));
  }, [data, q]);

  // Group by day
  const groups = useMemo(() => {
    const m = new Map<string, Session[]>();
    for (const h of list) {
      const day = String(h.date || "").slice(0, 10) || "Earlier";
      if (!m.has(day)) m.set(day, []);
      m.get(day)!.push(h);
    }
    return [...m.entries()];
  }, [list]);

  return (
    <>
      <PageHeader title="History" description="Every download session, newest first.">
        <div className="relative w-72">
          <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input className="h-8 pl-8" placeholder="Search history…" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
      </PageHeader>
      {list.length === 0 ? (
        <Empty icon={HistoryIcon} title="No downloads yet" description="Finished download sessions show up here." />
      ) : (
        <PageBody className="flex flex-col gap-6">
          {groups.map(([day, items]) => (
            <section key={day}>
              <h2 className="mb-2 text-xs font-medium text-muted-foreground">{day}</h2>
              <div className="overflow-hidden rounded-xl border">
                {items.map((h, i) => (
                  <div key={i} className="group flex items-center gap-4 border-b px-4 py-3 last:border-b-0 hover:bg-accent/30">
                    <div className="min-w-0 flex-1">
                      <div className="truncate font-medium">{h.creator || "Unknown creator"}</div>
                      <div className="truncate text-xs text-muted-foreground" data-selectable>{h.url}</div>
                    </div>
                    {h.service && <Badge variant="outline" className="capitalize">{h.service}</Badge>}
                    <span className="w-20 text-right text-sm text-muted-foreground tabular-nums">{h.files} files</span>
                    <span className="w-12 text-right text-xs text-muted-foreground tabular-nums">{String(h.date).slice(11)}</span>
                    <Tip content="Download again">
                      <Button variant="ghost" size="icon-sm" className="opacity-0 group-hover:opacity-100"
                        onClick={async () => { await setProp("appBridge", "currentUrl", h.url); call("appBridge", "startDownload"); }}>
                        <RotateCwIcon />
                      </Button>
                    </Tip>
                  </div>
                ))}
              </div>
            </section>
          ))}
        </PageBody>
      )}
    </>
  );
}
