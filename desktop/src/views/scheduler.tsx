import { useMemo, useState } from "react";
import { CalendarClockIcon, EllipsisIcon, MoonIcon, PencilIcon, PlayIcon, PlusIcon, RepeatIcon, TrashIcon, UserRoundIcon, BookmarkIcon } from "lucide-react";
import { toast } from "sonner";
import { call, useProp } from "@/lib/engine";
import {
  Badge, Button, Card, CardContent, CardDescription, CardHeader, CardTitle, Dialog, DialogContent, DialogFooter,
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger, Empty, Input, Label,
  Segmented, Switch,
} from "@/components/ui";
import { Field, PageBody, PageHeader } from "@/components/page";
import { cn } from "@/lib/utils";

type Schedule = {
  id: string; name: string; target_type: "watchlist" | "creator"; target_url: string; trigger_type: "interval" | "time_of_day";
  interval_hours: number; time_of_day: string; enabled: boolean; last_run: string | null; next_run: string | null; status: string;
};

function relative(iso: string | null): string {
  if (!iso) return "—";
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "—";
  const diff = t - Date.now();
  const abs = Math.abs(diff);
  const mins = Math.round(abs / 60000);
  const text = mins < 1 ? "now" : mins < 60 ? `${mins} min` : mins < 1440 ? `${Math.round(mins / 60)} h` : `${Math.round(mins / 1440)} d`;
  if (text === "now") return "now";
  return diff >= 0 ? `in ${text}` : `${text} ago`;
}

function useSchedulerSettings() {
  const enabled = useProp<boolean>("scheduler", "enabled", false);
  const lock = useProp<boolean>("scheduler", "lockThreadsDelay", false);
  const nightOwl = useProp<boolean>("scheduler", "nightOwlEnabled", false);
  const start = useProp<string>("scheduler", "nightOwlStart", "01:00");
  const end = useProp<string>("scheduler", "nightOwlEnd", "07:00");
  const preventSleep = useProp<boolean>("scheduler", "preventSleep", false);
  const sweep = useProp<boolean>("scheduler", "sweepRetry", false);
  const s = { enabled, lock, nightOwl, start, end, preventSleep, sweep };
  const save = (patch: Partial<typeof s>) => {
    const n = { ...s, ...patch };
    call("scheduler", "setSettings", n.enabled, n.lock, n.nightOwl, n.start, n.end, n.preventSleep, n.sweep);
  };
  return [s, save] as const;
}

export function SchedulerView() {
  const json = useProp<string>("scheduler", "schedulesJson", "[]");
  const schedules = useMemo<Schedule[]>(() => { try { return JSON.parse(json || "[]"); } catch { return []; } }, [json]);
  const [settings, save] = useSchedulerSettings();
  const [editing, setEditing] = useState<Schedule | "new" | null>(null);

  return (
    <>
      <PageHeader title="Scheduler" description="Run watchlist checks and creator downloads automatically."
        actions={<Button size="sm" onClick={() => setEditing("new")}><PlusIcon />New schedule</Button>} />
      <PageBody>
        <div className="mx-auto flex max-w-4xl flex-col gap-5">
          <Card className="gap-2">
            <CardHeader>
              <CardTitle>Automation</CardTitle>
              <CardDescription>{settings.enabled ? "Schedules run while the app is open." : "Schedules are paused."}</CardDescription>
            </CardHeader>
            <CardContent className="divide-y">
              <Field label="Run schedules"><Switch checked={settings.enabled} onCheckedChange={(v) => save({ enabled: v })} /></Field>
              <Field label="Night hours only" hint="Hold scheduled runs until this window.">
                <Input type="time" className="w-32" disabled={!settings.nightOwl} value={settings.start} onChange={(e) => save({ start: e.target.value })} />
                <span className="text-sm text-muted-foreground">to</span>
                <Input type="time" className="w-32" disabled={!settings.nightOwl} value={settings.end} onChange={(e) => save({ end: e.target.value })} />
                <Switch checked={settings.nightOwl} onCheckedChange={(v) => save({ nightOwl: v })} />
              </Field>
              <Field label="Gentle pace" hint="Lock threads and keep delays during scheduled runs."><Switch checked={settings.lock} onCheckedChange={(v) => save({ lock: v })} /></Field>
              <Field label="Keep the computer awake" hint="While a schedule is waiting or running."><Switch checked={settings.preventSleep} onCheckedChange={(v) => save({ preventSleep: v })} /></Field>
              <Field label="Retry failures afterwards" hint="Sweep the queue for failed files when a run ends."><Switch checked={settings.sweep} onCheckedChange={(v) => save({ sweep: v })} /></Field>
            </CardContent>
          </Card>

          <section className="flex flex-col gap-3">
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-medium">Schedules</h2>
              <span className="text-xs text-muted-foreground">{schedules.filter((s) => s.enabled).length} active</span>
            </div>
            {schedules.length === 0 ? (
              <Card className="py-0">
                <Empty icon={CalendarClockIcon} title="No schedules yet" description="Check your watchlist every few hours, or grab a creator's new posts every night.">
                  <Button size="sm" onClick={() => setEditing("new")}><PlusIcon />New schedule</Button>
                </Empty>
              </Card>
            ) : (
              <div className="@container/list overflow-hidden rounded-xl border">
                {schedules.map((s) => <ScheduleRow key={s.id} s={s} onEdit={() => setEditing(s)} />)}
              </div>
            )}
          </section>
        </div>
      </PageBody>
      <ScheduleDialog value={editing} onClose={() => setEditing(null)} />
    </>
  );
}

function ScheduleRow({ s, onEdit }: { s: Schedule; onEdit: () => void }) {
  const Icon = s.target_type === "watchlist" ? BookmarkIcon : UserRoundIcon;
  const when = s.trigger_type === "time_of_day" ? `Daily at ${s.time_of_day}` : `Every ${s.interval_hours} h`;
  const running = s.status === "running";
  return (
    <div className={cn("group flex items-center gap-4 border-b px-4 py-3 last:border-b-0 hover:bg-accent/30", !s.enabled && "opacity-60")}>
      <div className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-card"><Icon className="size-4 text-muted-foreground" /></div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-sm font-medium">{s.name}</span>
          {running && <Badge variant="info">Running</Badge>}
          {s.status === "error" && <Badge variant="destructive">Last run failed</Badge>}
        </div>
        <div className="mt-0.5 flex items-center gap-1.5 truncate text-xs text-muted-foreground">
          {s.trigger_type === "time_of_day" ? <MoonIcon className="size-3" /> : <RepeatIcon className="size-3" />}
          {when} · {s.target_type === "watchlist" ? "Watchlist check" : s.target_url}
        </div>
      </div>
      <div className="hidden w-28 flex-col text-right text-xs @lg/list:flex">
        <span className="text-muted-foreground">Next run</span>
        <span className="tabular-nums">{s.enabled ? relative(s.next_run) : "Off"}</span>
      </div>
      <Switch checked={s.enabled} onCheckedChange={(v) => call("scheduler", "toggleTask", s.id, v)} />
      <DropdownMenu>
        <DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm"><EllipsisIcon /></Button></DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem onSelect={onEdit}><PencilIcon />Edit</DropdownMenuItem>
          {s.target_type === "watchlist" && (
            <DropdownMenuItem onSelect={() => { call("appBridge", "checkWatchlist", false); toast.success("Checking the watchlist"); }}><PlayIcon />Run now</DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onSelect={() => call("scheduler", "deleteTask", s.id)}><TrashIcon />Delete</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

function ScheduleDialog({ value, onClose }: { value: Schedule | "new" | null; onClose: () => void }) {
  const open = value !== null;
  const initial = value && value !== "new" ? value : null;
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      {open && <ScheduleForm key={initial?.id ?? "new"} initial={initial} onClose={onClose} />}
    </Dialog>
  );
}

function ScheduleForm({ initial, onClose }: { initial: Schedule | null; onClose: () => void }) {
  const [name, setName] = useState(initial?.name ?? "");
  const [target, setTarget] = useState<"watchlist" | "creator">(initial?.target_type ?? "watchlist");
  const [url, setUrl] = useState(initial?.target_url ?? "");
  const [trigger, setTrigger] = useState<"interval" | "time_of_day">(initial?.trigger_type ?? "interval");
  const [hours, setHours] = useState(initial?.interval_hours ?? 6);
  const [time, setTime] = useState(initial?.time_of_day ?? "03:00");
  const blocked = /(kemono|coomer)\./i.test(url);
  const valid = target === "watchlist" || (url.trim().length > 0 && !blocked);

  const submit = async () => {
    const n = name.trim() || (target === "watchlist" ? "Watchlist check" : "Creator download");
    if (initial) await call("scheduler", "updateTask", initial.id, n, target, url.trim(), trigger, Math.max(1, hours), time);
    else await call("scheduler", "addTask", n, target, url.trim(), trigger, Math.max(1, hours), time);
    toast.success(initial ? "Schedule updated" : "Schedule created");
    onClose();
  };

  return (
    <DialogContent title={initial ? "Edit schedule" : "New schedule"} description="Runs while the app is open.">
      <div className="flex flex-col gap-4">
        <div className="flex flex-col gap-2">
          <Label>Name</Label>
          <Input placeholder={target === "watchlist" ? "Watchlist check" : "Creator download"} value={name} onChange={(e) => setName(e.target.value)} />
        </div>
        <div className="flex flex-col gap-2">
          <Label>What to run</Label>
          <Segmented value={target} onChange={setTarget} options={[
            { value: "watchlist", label: <><BookmarkIcon className="size-3.5" />Check the watchlist</> },
            { value: "creator", label: <><UserRoundIcon className="size-3.5" />Download a creator</> },
          ]} />
          {target === "creator" && (
            <>
              <Input placeholder="https://pawchive.pw/patreon/user/…" value={url} onChange={(e) => setUrl(e.target.value)} className={cn(blocked && "border-destructive/60")} />
              {blocked && <p className="text-xs text-destructive">This site isn't supported.</p>}
            </>
          )}
        </div>
        <div className="flex flex-col gap-2">
          <Label>When</Label>
          <div className="flex flex-wrap items-center gap-2">
            <Segmented value={trigger} onChange={setTrigger} options={[
              { value: "interval", label: "Every few hours" },
              { value: "time_of_day", label: "Once a day" },
            ]} />
            {trigger === "interval" ? (
              <div className="flex items-center gap-2">
                <Input type="number" min={1} max={168} className="h-8 w-20" value={hours} onChange={(e) => setHours(Math.max(1, Number(e.target.value) || 1))} />
                <span className="text-sm text-muted-foreground">hours</span>
              </div>
            ) : (
              <Input type="time" className="h-8 w-32" value={time} onChange={(e) => setTime(e.target.value)} />
            )}
          </div>
        </div>
      </div>
      <DialogFooter>
        <Button variant="outline" onClick={onClose}>Cancel</Button>
        <Button disabled={!valid} onClick={submit}>{initial ? "Save" : "Create"}</Button>
      </DialogFooter>
    </DialogContent>
  );
}
