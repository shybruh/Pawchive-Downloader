import { useEffect, useState, useSyncExternalStore } from "react";
import { FileIcon, FilmIcon, ImageIcon, KeyRoundIcon, LayoutGridIcon, Loader2Icon, LockKeyholeIcon, MusicIcon, PhoneIcon, QrCodeIcon, SendIcon, ShieldAlertIcon, ZapIcon } from "lucide-react";
import { toast } from "sonner";
import { call, setProp, useProp, useSignal } from "@/lib/engine";
import { Alert, Button, Dialog, DialogContent, DialogFooter, Input, Label, Segmented, Switch } from "@/components/ui";
import { cn } from "@/lib/utils";

const T = "telegramBridge";

// ── Sign-in dialog, openable from anywhere ──
let signInOpen = false;
let resumeAfter = false;
const listeners = new Set<() => void>();
export function openTelegramSignIn(resume = false) { signInOpen = true; resumeAfter = resume; listeners.forEach((l) => l()); }
function closeSignIn() { signInOpen = false; listeners.forEach((l) => l()); }
const useSignInOpen = () => useSyncExternalStore((l) => { listeners.add(l); return () => { listeners.delete(l); }; }, () => signInOpen);

type Method = "qr" | "phone" | "bot";

export function TelegramSignIn() {
  const open = useSignInOpen();
  const state = useProp<string>(T, "loginState", "idle");
  const status = useProp<string>(T, "statusMessage", "");
  const qr = useProp<string>(T, "qrCodeDataUrl", "");
  const loggedIn = useProp<boolean>(T, "isLoggedIn", false);
  const [method, setMethod] = useState<Method>("qr");
  const [phone, setPhone] = useState("");
  const [code, setCode] = useState("");
  const [password, setPassword] = useState("");
  const [token, setToken] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [apiId, setApiId] = useState("");
  const [apiHash, setApiHash] = useState("");
  // Two-step verification can follow any way of signing in (QR code or phone): it gets its own
  // step, and stays there after a wrong password instead of starting over
  const [needs2fa, setNeeds2fa] = useState(false);
  const [checking2fa, setChecking2fa] = useState(false);

  useSignal("appBridge", "telegramAuthRequested", () => openTelegramSignIn(true));

  useEffect(() => {
    if (state === "need_2fa") { setNeeds2fa(true); setPassword(""); }
    if (state === "connected" || state === "idle") setNeeds2fa(false);
  }, [state]);
  // Each try passes through "Contacting Telegram…", so a repeated wrong password (same state,
  // same message) still ends the spinner
  useEffect(() => {
    if (!/contacting/i.test(status)) setChecking2fa(false);
  }, [state, status]);

  const submitPassword = () => {
    if (!password || checking2fa) return;
    setChecking2fa(true);
    call(T, "submit2faPassword", password);
  };
  const startOver = () => {
    setNeeds2fa(false);
    setPassword("");
    call(T, "cancelQrLogin").catch(() => {});
    if (method === "qr") setTimeout(() => call(T, "startQrLogin").catch(() => {}), 50);
  };

  // Start QR sign-in as soon as that tab is shown
  // and stop it when switching to another way of signing in
  useEffect(() => {
    if (!open || loggedIn) return;
    if (method === "qr" && (state === "idle" || state === "error")) call(T, "startQrLogin").catch(() => {});
    if (method !== "qr" && state.startsWith("qr")) call(T, "cancelQrLogin").catch(() => {});
  }, [open, method]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (open && loggedIn) {
      toast.success("Signed in to Telegram");
      closeSignIn();
      if (resumeAfter) call("appBridge", "resumeTelegramAction").catch(() => {});
    }
  }, [open, loggedIn]);

  const close = () => {
    if (state.startsWith("qr") || needs2fa) call(T, "cancelQrLogin").catch(() => {});
    setNeeds2fa(false);
    closeSignIn();
  };
  const busy = state === "qr_generating" || state === "checking";

  return (
    <Dialog open={open} onOpenChange={(o) => !o && close()}>
      <DialogContent title="Sign in to Telegram" description="Needed to read channels. Your session is stored encrypted on this computer.">
        {needs2fa ? (
          <div className="flex flex-col gap-3">
            <div className="flex items-start gap-3 rounded-lg border bg-muted/40 p-3">
              <LockKeyholeIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
              <p className="text-sm text-muted-foreground">
                This account has two-step verification. Enter your Telegram cloud password (the one you set yourself, not a code).
              </p>
            </div>
            <div className="flex flex-col gap-2">
              <Label htmlFor="tg-2fa">Password</Label>
              <div className="flex gap-2">
                <Input id="tg-2fa" autoFocus type="password" autoComplete="current-password" value={password}
                  onChange={(e) => setPassword(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter") submitPassword(); }} />
                <Button disabled={!password || checking2fa} onClick={submitPassword}>
                  {checking2fa && <Loader2Icon className="animate-spin" />}Continue
                </Button>
              </div>
            </div>
            {state === "error" && status && <p className="text-xs text-destructive">{status}</p>}
            <Button variant="ghost" size="sm" className="self-start" onClick={startOver}>Start over</Button>
          </div>
        ) : (<>
        <Segmented value={method} onChange={setMethod} options={[
          { value: "qr", label: <><QrCodeIcon className="size-3.5" />QR code</> },
          { value: "phone", label: <><PhoneIcon className="size-3.5" />Phone number</> },
          { value: "bot", label: <><KeyRoundIcon className="size-3.5" />Bot token</> },
        ]} />

        {method === "qr" && (
          <div className="flex items-center gap-5">
            <div className="flex size-44 shrink-0 items-center justify-center rounded-xl bg-white p-2">
              {qr && state === "qr_ready" ? <img src={qr} className="size-full" alt="Telegram sign-in QR code" /> : <Loader2Icon className="size-6 animate-spin text-neutral-500" />}
            </div>
            <ol className="list-decimal space-y-1.5 pl-4 text-sm text-muted-foreground">
              <li>Open Telegram on your phone</li>
              <li>Go to Settings → Devices → Link desktop device</li>
              <li>Scan this code</li>
            </ol>
          </div>
        )}

        {method === "phone" && (
          <div className="flex flex-col gap-3">
            {state !== "code_sent" && (
              <div className="flex flex-col gap-2">
                <Label>Phone number (with country code)</Label>
                <div className="flex gap-2">
                  <Input autoFocus placeholder="+1 555 123 4567" value={phone} onChange={(e) => setPhone(e.target.value)} />
                  <Button disabled={!phone.trim() || busy} onClick={() => call(T, "sendPhoneCode", phone.trim())}>Send code</Button>
                </div>
              </div>
            )}
            {state === "code_sent" && (
              <div className="flex flex-col gap-2">
                <Label>Code from Telegram</Label>
                <div className="flex gap-2">
                  <Input autoFocus inputMode="numeric" placeholder="12345" value={code} onChange={(e) => setCode(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Enter" && code.trim()) call(T, "verifyPhoneCode", code.trim()); }} />
                  <Button disabled={!code.trim()} onClick={() => call(T, "verifyPhoneCode", code.trim())}>Sign in</Button>
                </div>
              </div>
            )}
          </div>
        )}

        {method === "bot" && (
          <div className="flex flex-col gap-2">
            <Label>Bot token</Label>
            <div className="flex gap-2">
              <Input type="password" placeholder="123456:ABC-DEF…" value={token} onChange={(e) => setToken(e.target.value)} className="font-mono text-[13px]" />
              <Button disabled={!token.trim()} onClick={() => call(T, "loginWithBotToken", token.trim())}>Sign in</Button>
            </div>
            <p className="text-xs text-muted-foreground">Bots can only read public channels they've been added to.</p>
          </div>
        )}

        {status && (method === "qr" || !state.startsWith("qr")) && (
          <p className={cn("text-xs", state === "error" ? "text-destructive" : "text-muted-foreground")}>{status}</p>
        )}
        </>)}

        <div className="border-t pt-3">
          <label className="flex items-center justify-between text-sm">
            <span className="text-muted-foreground">Use my own API ID and hash</span>
            <Switch checked={advanced} onCheckedChange={setAdvanced} />
          </label>
          {advanced && (
            <div className="mt-3 flex gap-2">
              <Input placeholder="API ID" inputMode="numeric" value={apiId} onChange={(e) => setApiId(e.target.value.replace(/\D/g, ""))} className="w-32" />
              <Input placeholder="API hash" value={apiHash} onChange={(e) => setApiHash(e.target.value)} className="font-mono text-[13px]" />
              <Button variant="outline" disabled={!apiId || !apiHash.trim()} onClick={() => { call(T, "setCustomCredentials", Number(apiId), apiHash.trim()); toast.success("API credentials saved"); }}>Save</Button>
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

// ── Scope: how much of a channel to fetch ──
type Scope = { channelId: string; rawUrl: string; isPrivate: boolean; action: string };
const TYPES = [
  { key: "video", label: "Videos", icon: FilmIcon },
  { key: "photo", label: "Photos", icon: ImageIcon },
  { key: "document", label: "Files", icon: FileIcon },
  { key: "audio", label: "Audio", icon: MusicIcon },
];

export function TelegramScope() {
  const [scope, setScope] = useState<Scope | null>(null);
  const [title, setTitle] = useState("");
  const [limit, setLimit] = useState(200);
  const [types, setTypes] = useState<Set<string>>(new Set(["video", "photo"]));
  const [maxMb, setMaxMb] = useState(0);
  const [loading, setLoading] = useState<"select" | "download" | null>(null);
  const [error, setError] = useState("");
  const safety = useProp<boolean>("appBridge", "telegramSafetyAcknowledged", false);
  const liability = useProp<boolean>("appBridge", "telegramLiabilityAcknowledged", false);
  const acknowledged = safety && liability;

  useSignal("appBridge", "telegramScopeRequested", (channelId: string, rawUrl: string, isPrivate: boolean, action: string) => {
    setScope({ channelId: String(channelId), rawUrl: String(rawUrl), isPrivate: !!isPrivate, action: String(action || "download") });
    setTitle(""); setError(""); setLoading(null);
    call(T, "resolveTarget", String(channelId), !!isPrivate).catch(() => {});
  });
  useSignal(T, "channelMetadataReady", (meta: { title?: string }) => { if (meta?.title) setTitle(meta.title); });
  useSignal(T, "channelResolutionFailed", (err: string) => { setError(String(err)); setLoading(null); });
  useSignal(T, "channelMessagesReady", (messages: unknown[]) => {
    if (!scope) return;
    const name = title || scope.channelId;
    if (!messages?.length) { setError("No matching media in that range."); setLoading(null); return; }
    call("appBridge", "queueTelegramFiles", name, messages, true);
    toast.success(`Queued ${messages.length} file${messages.length === 1 ? "" : "s"} from ${name}`);
    setScope(null);
  });
  useSignal(T, "channelMessagesForSelectionReady", (messages: unknown[]) => {
    if (!scope) return;
    if (!messages?.length) { setError("No matching media in that range."); setLoading(null); return; }
    call("appBridge", "openTelegramPostSelection", title || scope.channelId, messages, scope.rawUrl);
    setScope(null);
  });

  const fetch = (forSelection: boolean) => {
    if (!scope) return;
    setError(""); setLoading(forSelection ? "select" : "download");
    call(T, "fetchMessages", scope.channelId, limit, [...types], maxMb, forSelection);
  };
  const toggle = (k: string) => setTypes((s) => { const n = new Set(s); if (n.has(k)) n.delete(k); else n.add(k); return n; });

  return (
    <Dialog open={!!scope} onOpenChange={(o) => !o && setScope(null)}>
      <DialogContent title={title ? `Telegram — ${title}` : "Telegram channel"} description="Choose how much of the channel to get.">
        {!acknowledged ? (
          <>
            <Alert tone="warning" icon={ShieldAlertIcon}>
              <div className="space-y-2">
                <p>Telegram can limit or ban accounts that read lots of channels quickly. Use an account you can afford to lose, and keep ranges reasonable.</p>
                <p>You're responsible for having the right to download what you fetch.</p>
              </div>
            </Alert>
            <DialogFooter>
              <Button variant="outline" onClick={() => setScope(null)}>Cancel</Button>
              <Button onClick={() => { setProp("appBridge", "telegramSafetyAcknowledged", true); setProp("appBridge", "telegramLiabilityAcknowledged", true); call(T, "acceptDisclaimer").catch(() => {}); }}>I understand</Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <div className="flex flex-col gap-4">
              <div className="flex flex-col gap-2">
                <Label>Latest posts to look through</Label>
                <div className="flex flex-wrap items-center gap-2">
                  <Input type="number" min={1} max={100000} className="w-28" value={limit} onChange={(e) => setLimit(Math.max(1, Number(e.target.value) || 1))} />
                  {[50, 200, 1000, 5000].map((n) => (
                    <Button key={n} size="xs" variant={limit === n ? "secondary" : "ghost"} onClick={() => setLimit(n)}>{n.toLocaleString()}</Button>
                  ))}
                </div>
              </div>
              <div className="flex flex-col gap-2">
                <Label>Media</Label>
                <div className="grid grid-cols-4 gap-2">
                  {TYPES.map(({ key, label, icon: Icon }) => (
                    <button key={key} onClick={() => toggle(key)}
                      className={cn("flex flex-col items-center gap-1.5 rounded-lg border px-2 py-2.5 text-xs font-medium transition-colors",
                        types.has(key) ? "border-primary/60 bg-primary/5 ring-1 ring-primary/30" : "text-muted-foreground hover:bg-accent/50")}>
                      <Icon className="size-4" />{label}
                    </button>
                  ))}
                </div>
              </div>
              <div className="flex items-center justify-between gap-4">
                <Label>Largest file (MB)</Label>
                <Input type="number" min={0} className="w-28" placeholder="No limit" value={maxMb || ""} onChange={(e) => setMaxMb(Math.max(0, Number(e.target.value) || 0))} />
              </div>
              {error && <Alert tone="destructive" icon={SendIcon}>{error}</Alert>}
            </div>
            <DialogFooter>
              <Button variant="outline" onClick={() => setScope(null)}>Cancel</Button>
              <Button variant="outline" disabled={!types.size || !!loading} onClick={() => fetch(true)}>
                {loading === "select" ? <Loader2Icon className="animate-spin" /> : <LayoutGridIcon />}Choose posts…
              </Button>
              <Button disabled={!types.size || !!loading} onClick={() => fetch(false)}>
                {loading === "download" ? <Loader2Icon className="animate-spin" /> : <ZapIcon />}Download
              </Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
