import { useEffect, useState, type ReactNode } from "react";
import {
  CircleCheckIcon, CookieIcon, DownloadCloudIcon, ExternalLinkIcon, FolderOpenIcon, GaugeIcon, KeyRoundIcon,
  InfoIcon, Loader2Icon, LogInIcon, LogOutIcon, PaletteIcon, RefreshCwIcon, ShieldIcon, TypeIcon, UserRoundIcon,
} from "lucide-react";
import { inTauri, openExternal, siteLogin, siteLoginCancel, siteLoginDone } from "@/lib/native";
import { openTelegramSignIn } from "@/components/telegram";
import { useNav } from "@/lib/nav";
import { checkForUpdate, installUpdate, useUpdater } from "@/lib/updater";
import logo from "@/assets/logo.png";
import { toast } from "sonner";
import { call, useProp, useQuery, useSetting, useSignal } from "@/lib/engine";
import { pickFolder } from "@/lib/native";
import { Alert, Badge, Button, Card, Progress, CardContent, CardDescription, CardHeader, CardTitle, Input, Select, Separator, Switch } from "@/components/ui";
import { Field, PageHeader } from "@/components/page";
import { cn } from "@/lib/utils";

type Section = "downloads" | "network" | "files" | "accounts" | "appearance" | "updates" | "about";

const SECTIONS: { id: Section; label: string; icon: typeof GaugeIcon }[] = [
  { id: "downloads", label: "Downloads", icon: DownloadCloudIcon },
  { id: "files", label: "Files & naming", icon: TypeIcon },
  { id: "network", label: "Network", icon: GaugeIcon },
  { id: "accounts", label: "Accounts", icon: UserRoundIcon },
  { id: "appearance", label: "Appearance", icon: PaletteIcon },
  { id: "updates", label: "Updates", icon: RefreshCwIcon },
  { id: "about", label: "About", icon: InfoIcon },
];

export function SettingsView() {
  const { params } = useNav();
  const [section, setSection] = useState<Section>((params.section as Section) || "downloads");
  useEffect(() => { if (params.section) setSection(params.section as Section); }, [params.section]);
  return (
    <>
      <PageHeader title="Settings" description="Changes are saved automatically." />
      <div className="flex min-h-0 flex-1">
        <nav className="flex w-52 shrink-0 flex-col gap-0.5 border-r p-3">
          {SECTIONS.map((s) => {
            const Icon = s.icon;
            return (
              <button key={s.id} onClick={() => setSection(s.id)}
                className={cn("flex h-8 items-center gap-2.5 rounded-md px-2 text-sm",
                  section === s.id ? "bg-accent font-medium" : "text-muted-foreground hover:bg-accent/50 hover:text-foreground")}>
                <Icon className="size-4" />{s.label}
              </button>
            );
          })}
        </nav>
        <div className="min-h-0 flex-1 overflow-y-auto">
          <div className="mx-auto flex max-w-3xl flex-col gap-5 px-8 py-6">
            {section === "downloads" && <DownloadsSection />}
            {section === "files" && <FilesSection />}
            {section === "network" && <NetworkSection />}
            {section === "accounts" && <AccountsSection />}
            {section === "appearance" && <AppearanceSection />}
            {section === "updates" && <UpdatesSection />}
            {section === "about" && <AboutSection />}
          </div>
        </div>
      </div>
    </>
  );
}

function Group({ title, description, children }: { title: string; description?: string; children: ReactNode }) {
  return (
    <Card className="gap-2">
      <CardHeader><CardTitle>{title}</CardTitle>{description && <CardDescription>{description}</CardDescription>}</CardHeader>
      <CardContent className="divide-y">{children}</CardContent>
    </Card>
  );
}

function TextSetting({ prop, placeholder, className, mono }: { prop: string; placeholder?: string; className?: string; mono?: boolean }) {
  const [v, set] = useSetting<string>("appBridge", prop, "");
  return <Input className={cn("w-64", mono && "font-mono text-[13px]", className)} placeholder={placeholder} value={v ?? ""} onChange={(e) => set(e.target.value)} />;
}

function SelectSetting({ prop, options }: { prop: string; options: { value: string; label: string }[] }) {
  const [v, set] = useSetting<string>("appBridge", prop, options[0]?.value ?? "");
  return <Select value={String(v ?? "")} onValueChange={set} options={options} className="w-56" />;
}

function SwitchSetting({ prop }: { prop: string }) {
  const [v, set] = useSetting<boolean>("appBridge", prop, false);
  return <Switch checked={!!v} onCheckedChange={set} />;
}

function DownloadsSection() {
  const [dir, setDir] = useSetting<string>("appBridge", "downloadDir", "");
  return (
    <>
      <Group title="Location">
        <Field label="Download folder" hint={<span className="block truncate font-mono text-xs" title={dir} data-selectable>{dir}</span>}>
          <Button variant="outline" size="sm" onClick={async () => { const d = await pickFolder("Download folder", dir); if (d) setDir(d); }}><FolderOpenIcon />Change</Button>
        </Field>
        <Field label="Open the folder when a download finishes"><SwitchSetting prop="openFolderOnComplete" /></Field>
        <Field label="Play a sound when finished"><SwitchSetting prop="playCompletionSound" /></Field>
      </Group>
      <Group title="Behaviour">
        <Field label="Retry failed files at the end"><SwitchSetting prop="autoRetryAtEnd" /></Field>
        <Field label="Don't retry files that are gone (404)"><SwitchSetting prop="skipRetry404" /></Field>
        <Field label="Delete failed downloads' leftovers" hint="Removes the unfinished part of failed or cancelled files when the queue ends."><SwitchSetting prop="autoDeleteFailed" /></Field>
        <Field label="Re-download files that look too small"><SwitchSetting prop="redownloadSmallFiles" /></Field>
        <Field label="Keep a download archive" hint="Skips files you already have, even after moving them."><SwitchSetting prop="enableDownloadArchive" /></Field>
        <Field label="When the queue finishes">
          <SelectSetting prop="postDownloadAction" options={[
            { value: "none", label: "Do nothing" }, { value: "close_app", label: "Close the app" }, { value: "sleep", label: "Sleep" },
            { value: "hibernate", label: "Hibernate" }, { value: "shutdown", label: "Shut down" }, { value: "restart", label: "Restart" },
          ]} />
        </Field>
      </Group>
      <Group title="Telegram">
        <Field label="Telegram account" hint="Sign-in for Telegram channels is handled in Accounts."><Badge variant="outline">Accounts</Badge></Field>
      </Group>
    </>
  );
}

function FilesSection() {
  const [style] = useSetting<string>("appBridge", "filenameStyle", "post_title");
  return (
    <>
      <Group title="File names">
        <Field label="Naming style">
          <SelectSetting prop="filenameStyle" options={[
            { value: "post_title", label: "Post title" }, { value: "date_post_title", label: "Date + post title" },
            { value: "date_based", label: "Date only" }, { value: "post_title_global_numbering", label: "Post title, numbered" },
            { value: "custom", label: "Custom template" },
          ]} />
        </Field>
        {style === "custom" && <Field label="Template" hint="e.g. {date}_{title}_{index}"><TextSetting prop="filenameTemplate" mono /></Field>}
        <Field label="Date prefix"><SwitchSetting prop="datePrefix" /></Field>
        <Field label="Number files within a post"><SwitchSetting prop="fileIndexPrefix" /></Field>
        <Field label="File order">
          <SelectSetting prop="fileOrder" options={[{ value: "posted", label: "As posted" }, { value: "reversed", label: "Reversed" }, { value: "name", label: "By file name" }]} />
        </Field>
      </Group>
      <Group title="Folders">
        <Field label="Folder per post"><SwitchSetting prop="subfolderPerPost" /></Field>
        <Field label="Site name in folder names"><SwitchSetting prop="siteInFolderName" /></Field>
        <Field label="Sort known series into folders"><SwitchSetting prop="separateFoldersByKnown" /></Field>
      </Group>
      <Group title="Extras">
        <Field label="Save post text and metadata"><SwitchSetting prop="savePostMetadata" /></Field>
        <Field label="Write sidecar files"><SwitchSetting prop="writeSidecars" /></Field>
        <Field label="Sidecar format">
          <SelectSetting prop="sidecarFormat" options={[{ value: "json", label: "JSON" }, { value: "txt", label: "Text" }]} />
        </Field>
        <Field label="Tag audio files"><SwitchSetting prop="writeAudioMetadata" /></Field>
        <Field label="WebP quality">
          <SelectSetting prop="webpQuality" options={[
            { value: "lossless", label: "Lossless" }, { value: "high", label: "High" }, { value: "balanced", label: "Balanced" },
            { value: "small", label: "Small" }, { value: "smallest", label: "Smallest" },
          ]} />
        </Field>
      </Group>
    </>
  );
}

function NetworkSection() {
  const [threads, setThreads] = useSetting<number>("appBridge", "threadsCount", 4);
  const maxThreads = useProp<number>("appBridge", "maxCpuThreads", 16);
  const [delay, setDelay] = useSetting<number>("appBridge", "downloadDelay", 0);
  const bwEnabled = useProp<boolean>("appBridge", "bandwidthLimitEnabled", false);
  const bwValue = useProp<number>("appBridge", "bandwidthLimitValue", 10);
  const bwUnit = useProp<string>("appBridge", "bandwidthLimitUnit", "MB/s");
  return (
    <>
      <Group title="Speed">
        <Field label="Adaptive threading" hint="Scales parallel downloads up and down based on how the server responds."><SwitchSetting prop="adaptiveThreading" /></Field>
        <Field label="Parallel downloads" hint={`Up to ${maxThreads}`}>
          <Input type="number" min={1} max={maxThreads} className="w-24" value={threads} onChange={(e) => setThreads(Math.max(1, Math.min(maxThreads, Number(e.target.value) || 1)))} />
        </Field>
        <Field label="Delay between files" hint="Seconds">
          <Input type="number" min={0} step={0.5} className="w-24" value={delay} onChange={(e) => setDelay(Math.max(0, Number(e.target.value) || 0))} />
        </Field>
        <Field label="Bandwidth limit">
          <Switch checked={bwEnabled} onCheckedChange={(v) => call("appBridge", "setBandwidthLimit", v, Number(bwValue) || 10, bwUnit)} />
          <Input type="number" min={0.1} step={0.5} className="w-20" disabled={!bwEnabled} value={bwValue}
            onChange={(e) => call("appBridge", "setBandwidthLimit", bwEnabled, Number(e.target.value) || 0.1, bwUnit)} />
          <Select size="sm" value={bwUnit} onValueChange={(u) => call("appBridge", "setBandwidthLimit", bwEnabled, Number(bwValue) || 10, u)}
            options={[{ value: "KB/s", label: "KB/s" }, { value: "MB/s", label: "MB/s" }]} />
        </Field>
      </Group>
      <Group title="Connection">
        <Field label="Proxy" hint="http://, https:// or socks5://"><TextSetting prop="proxyUrl" placeholder="None" mono /></Field>
        <Field label="User agent"><TextSetting prop="userAgent" placeholder="Default" mono className="w-80" /></Field>
      </Group>
    </>
  );
}

type Provider = {
  id: string; name: string; domain: string; category: string; auth_type: string; status_text: string; logged_in?: boolean;
  is_logged_in?: boolean; masked_value?: string; supports_browser_import?: boolean; supports_credentials_login?: boolean; help_tip?: string;
};

function AccountsSection() {
  const { data, reload } = useQuery<Provider[]>("appBridge", "getProvidersList");
  useSignal("appBridge", "providersChanged", () => reload());
  const cookieText = useProp<string>("appBridge", "cookieWatchdogText", "");
  const providers = (data ?? []).filter((p) => !/kemono|coomer/i.test(p.id));
  return (
    <>
      {cookieText && (
        <Alert icon={ShieldIcon}>{cookieText}</Alert>
      )}
      {providers.filter((p) => p.id !== "telegram").map((p) => <ProviderCard key={p.id} p={p} />)}
      {providers.length === 0 && <div className="text-sm text-muted-foreground">Loading accounts…</div>}
      <Card className="gap-3">
        <CardHeader><CardTitle>Telegram</CardTitle><CardDescription>Needed to download from Telegram channels.</CardDescription></CardHeader>
        <TelegramAccount />
      </Card>
    </>
  );
}

// Login pages that open in the app's sign-in window
const LOGIN_PAGES: Record<string, string> = {
  pawchive: "https://pawchive.pw/account/login",
  cumst: "https://cum.st/auth/signin",
};

function ProviderCard({ p }: { p: Provider }) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [importing, setImporting] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  const [user, setUser] = useState("");
  const [pass, setPass] = useState("");
  const [loggingIn, setLoggingIn] = useState(false);
  const connected = !!(p.is_logged_in ?? p.logged_in) || /^connected/i.test(p.status_text || "");
  useSignal("appBridge", "providerValidationFinished", (id: string, ok: boolean, msg: string) => {
    if (id !== p.id) return;
    setBusy(false);
    if (ok) toast.success(msg || `${p.name} session is valid`); else toast.error(msg || `${p.name} session didn't work`);
  });
  useSignal("appBridge", "providerImportFinished", (id: string, ok: boolean, msg: string) => {
    if (id !== p.id) return;
    setImporting(false);
    if (ok) { toast.success(msg || "Imported"); setBusy(true); call("appBridge", "validateProviderSession", p.id); }
    else toast.error("Couldn't import from your browser", { description: msg, duration: 10000 });
  });
  const signInInApp = async () => {
    setSigningIn(true);
    try {
      const r = await siteLogin(LOGIN_PAGES[p.id]);
      // Closing the window without signing in leaves no session cookie: keep what was saved
      if (!r.names.some((n) => /session/i.test(n))) { toast.message("Not signed in", { description: "The window closed before a sign-in finished." }); return; }
      await call("appBridge", "saveProviderCredential", p.id, r.cookie);
      setBusy(true);
      call("appBridge", "validateProviderSession", p.id);
    } catch (e) {
      toast.error("Couldn't open the sign-in window", { description: String(e) });
    } finally {
      setSigningIn(false);
    }
  };
  useSignal("appBridge", "providerLoginFinished", (id: string, ok: boolean, msg: string) => {
    if (id !== p.id) return;
    setLoggingIn(false);
    if (ok) { setPass(""); toast.success(msg || `Signed in to ${p.name}`); } else toast.error(msg || "Sign-in failed");
  });
  return (
    <Card className="gap-3">
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          {p.id === "cumst" ? "cum.st" : p.name}
          {connected ? <Badge variant="success"><CircleCheckIcon />{p.status_text || "Connected"}</Badge> : <Badge variant="outline">Not connected</Badge>}
        </CardTitle>
        <CardDescription>{p.domain || p.category}</CardDescription>
      </CardHeader>
      {p.id !== "telegram" && (
        <CardContent className="flex flex-col gap-3">
          {p.supports_credentials_login && !connected && (
            <form className="flex gap-2" onSubmit={(e) => {
              e.preventDefault();
              if (!user.trim() || !pass) return;
              setLoggingIn(true);
              call("appBridge", "loginProviderWithCredentials", p.id, user.trim(), pass);
            }}>
              <Input placeholder="Username or email" autoComplete="username" value={user} onChange={(e) => setUser(e.target.value)} />
              <Input type="password" placeholder="Password" autoComplete="current-password" value={pass} onChange={(e) => setPass(e.target.value)} />
              <Button type="submit" disabled={loggingIn || !user.trim() || !pass}>{loggingIn && <Loader2Icon className="animate-spin" />}Sign in</Button>
            </form>
          )}
          {p.supports_credentials_login && !connected && <div className="text-xs text-muted-foreground">Or paste a session cookie:</div>}
          <div className="flex gap-2">
            <Input type="password" placeholder={connected ? (p.masked_value || "Saved — paste to replace") : p.auth_type === "token" ? "Paste your access token" : p.auth_type === "api_key" ? "Paste your API key" : "Paste a session cookie"}
              value={value} onChange={(e) => setValue(e.target.value)} className="font-mono text-[13px]" />
            <Button disabled={!value.trim()} onClick={async () => {
              await call("appBridge", "saveProviderCredential", p.id, value.trim()); setValue(""); toast.success(`${p.name} saved`);
            }}><KeyRoundIcon />Save</Button>
          </div>
          <div className="flex flex-wrap gap-2">
            {LOGIN_PAGES[p.id] && inTauri() && (signingIn ? (
              <>
                <Button size="sm" onClick={() => siteLoginDone()}><CircleCheckIcon />I've signed in</Button>
                <Button variant="ghost" size="sm" onClick={() => { siteLoginCancel(); setSigningIn(false); }}>Cancel</Button>
              </>
            ) : (
              <Button variant={connected ? "outline" : "default"} size="sm" onClick={signInInApp}><LogInIcon />{connected ? "Sign in again" : "Sign in on the site"}</Button>
            ))}
            {p.supports_browser_import && (
              <Button variant="outline" size="sm" disabled={importing} onClick={() => { setImporting(true); call("appBridge", "importBrowserCookiesToProvider", p.id, ""); }}
                title="Firefox works best; Chrome, Edge and Brave lock their cookies to themselves">
                {importing ? <Loader2Icon className="animate-spin" /> : <CookieIcon />}Import from browser
              </Button>
            )}
            {connected && (
              <>
                <Button variant="outline" size="sm" disabled={busy} onClick={() => { setBusy(true); call("appBridge", "validateProviderSession", p.id); }}>
                  {busy ? <Loader2Icon className="animate-spin" /> : <RefreshCwIcon />}Test
                </Button>
                <Button variant="ghost" size="sm" onClick={() => call("appBridge", "clearProviderCredential", p.id)}><LogOutIcon />Disconnect</Button>
              </>
            )}
          </div>
        </CardContent>
      )}
      {p.id === "telegram" && <TelegramAccount />}
    </Card>
  );
}

function TelegramAccount() {
  const loggedIn = useProp<boolean>("telegramBridge", "isLoggedIn", false);
  const user = useProp<string>("telegramBridge", "currentUsername", "");
  const phone = useProp<string>("telegramBridge", "currentPhone", "");
  return (
    <CardContent className="flex items-center justify-between gap-3">
      <span className="text-sm text-muted-foreground">
        {loggedIn ? `Signed in${user ? ` as @${user}` : phone ? ` as ${phone}` : ""}` : "Not signed in"}
      </span>
      {loggedIn ? (
        <Button variant="ghost" size="sm" onClick={() => call("telegramBridge", "disconnectAccount")}><LogOutIcon />Sign out</Button>
      ) : (
        <Button variant="outline" size="sm" onClick={() => openTelegramSignIn()}>Sign in</Button>
      )}
    </CardContent>
  );
}

function AppearanceSection() {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  return (
    <Group title="Appearance">
      <Field label="Dark mode">
        <Switch checked={dark} onCheckedChange={(v) => {
          setDark(v);
          document.documentElement.classList.toggle("dark", v);
          try { localStorage.setItem("theme", v ? "dark" : "light"); } catch { /* storage off */ }
        }} />
      </Field>
    </Group>
  );
}

function UpdatesSection() {
  const u = useUpdater();
  const busy = u.phase === "checking" || u.phase === "downloading" || u.phase === "installing";
  const label = u.phase === "available" ? `Version ${u.latest} is available`
    : u.phase === "downloading" ? `Downloading ${u.latest}…`
    : u.phase === "installing" ? "Installing — the app will restart"
    : u.phase === "none" ? "You're up to date" : "Check for updates";
  return (
    <Group title="Updates" description={`You're on version ${u.version || "—"}.`}>
      <Field label={label} hint={u.phase === "error" ? <span className="text-destructive">{u.error}</span> : undefined}>
        {u.phase === "available" ? (
          <Button size="sm" onClick={installUpdate}><DownloadCloudIcon />Update and restart</Button>
        ) : (
          <Button variant="outline" size="sm" disabled={busy} onClick={checkForUpdate}>
            {busy ? <Loader2Icon className="animate-spin" /> : <RefreshCwIcon />}Check now
          </Button>
        )}
      </Field>
      {u.phase === "downloading" && <div className="py-3"><Progress value={u.progress} /></div>}
      {(u.phase === "available" || u.phase === "downloading") && u.notes && (
        <>
          <Separator className="my-1" />
          <pre className="max-h-64 overflow-y-auto py-3 font-sans text-[13px] whitespace-pre-wrap text-muted-foreground" data-selectable>{u.notes}</pre>
        </>
      )}
    </Group>
  );
}

const UPSTREAM = "https://github.com/whyamihere773/Pawchive-Downloader";

function AboutSection() {
  const { version } = useUpdater();
  return (
    <>
      <Card>
        <CardContent className="flex items-center gap-4">
          <img src={logo} alt="" className="size-12 rounded-xl" />
          <div className="min-w-0 flex-1">
            <div className="font-semibold">Pawchive Downloader</div>
            <div className="text-sm text-muted-foreground">Version {version || "—"}</div>
          </div>
        </CardContent>
      </Card>
      <Group title="Credits">
        <div className="flex flex-col gap-3 py-3 text-sm">
          <p>
            Based on <button className="font-medium underline underline-offset-4" onClick={() => openExternal(UPSTREAM)}>Pawchive Downloader</button> by
            whyamihere773 and contributors. This version replaces the original Qt / QML interface with a new Tauri + React
            interface, runs the original Python engine in the background, and drops Kemono and Coomer support.
          </p>
          <p className="text-muted-foreground">
            The original project credits Yuvi9587's Kemono Downloader as its inspiration. It's shared under a custom
            source-available license (personal, non-commercial use) — see LICENSE.
          </p>
          <div>
            <Button variant="outline" size="sm" onClick={() => openExternal(UPSTREAM)}><ExternalLinkIcon />Original project</Button>
          </div>
        </div>
      </Group>
    </>
  );
}
