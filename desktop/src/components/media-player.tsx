import { useCallback, useEffect, useRef, useState } from "react";
import {
  GaugeIcon, Maximize2Icon, Minimize2Icon, PauseIcon, PictureInPicture2Icon, PlayIcon, RepeatIcon, RotateCcwIcon,
  RotateCwIcon, Volume1Icon, Volume2Icon, VolumeXIcon,
} from "lucide-react";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger, Tip } from "@/components/ui";
import { cn } from "@/lib/utils";

const SPEEDS = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 2];

function fmt(t: number) {
  if (!Number.isFinite(t) || t < 0) t = 0;
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = Math.floor(t % 60);
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
}

function stored<T>(key: string, fallback: T): T {
  try { const v = localStorage.getItem(key); return v === null ? fallback : (JSON.parse(v) as T); } catch { return fallback; }
}
function store(key: string, v: unknown) {
  try { localStorage.setItem(key, JSON.stringify(v)); } catch { /* storage off */ }
}

/**
 * Video player with its own controls (the browser's built-in ones look out of place in the app).
 * Keys: Space / K play · ← → 5 s · J L 10 s · ↑ ↓ volume · M mute · F fullscreen · 0–9 jump · , . frame
 */
export function MediaPlayer({ src, autoPlay = true, onError, className }: {
  src: string; autoPlay?: boolean; onError?: () => void; className?: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const video = useRef<HTMLVideoElement>(null);
  const bar = useRef<HTMLDivElement>(null);
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [buffered, setBuffered] = useState(0);
  const [volume, setVolume] = useState(() => stored("player.volume", 1));
  const [muted, setMuted] = useState(() => stored("player.muted", false));
  const [rate, setRate] = useState(1);
  const [loop, setLoop] = useState(() => stored("player.loop", false));
  const [full, setFull] = useState(false);
  const [showUi, setShowUi] = useState(true);
  const [hover, setHover] = useState<{ x: number; t: number } | null>(null);
  const [scrubbing, setScrubbing] = useState(false);
  const [flash, setFlash] = useState<string | null>(null);
  const hideTimer = useRef<number | null>(null);

  const v = () => video.current;

  // Keep settings on the element and remembered between videos
  useEffect(() => { const el = v(); if (el) { el.volume = volume; el.muted = muted; } store("player.volume", volume); store("player.muted", muted); }, [volume, muted]);
  useEffect(() => { const el = v(); if (el) el.loop = loop; store("player.loop", loop); }, [loop]);
  useEffect(() => { const el = v(); if (el) el.playbackRate = rate; }, [rate, src]);

  const poke = useCallback(() => {
    setShowUi(true);
    if (hideTimer.current) window.clearTimeout(hideTimer.current);
    hideTimer.current = window.setTimeout(() => { if (!v()?.paused) setShowUi(false); }, 2500);
  }, []);

  const toast = (text: string) => { setFlash(text); window.setTimeout(() => setFlash((f) => (f === text ? null : f)), 700); };
  const toggle = () => { const el = v(); if (!el) return; if (el.paused) el.play().catch(() => {}); else el.pause(); poke(); };
  const seekBy = (d: number) => { const el = v(); if (!el) return; el.currentTime = Math.max(0, Math.min(el.duration || 0, el.currentTime + d)); toast(`${d > 0 ? "+" : ""}${d}s`); poke(); };
  const setVol = (x: number) => { const n = Math.max(0, Math.min(1, x)); setVolume(n); setMuted(n === 0); toast(`${Math.round(n * 100)}%`); };
  const goFull = async () => {
    if (document.fullscreenElement) await document.exitFullscreen().catch(() => {});
    else await box.current?.requestFullscreen().catch(() => {});
  };

  useEffect(() => {
    const onFs = () => setFull(document.fullscreenElement === box.current);
    document.addEventListener("fullscreenchange", onFs);
    return () => document.removeEventListener("fullscreenchange", onFs);
  }, []);

  // Keyboard (capture, so the viewer's own arrow keys don't also fire while a video is shown)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || e.ctrlKey || e.metaKey || e.altKey) return;
      const el = v();
      if (!el) return;
      const k = e.key.toLowerCase();
      const handled = () => { e.preventDefault(); e.stopPropagation(); };
      if (k === " " || k === "k") { handled(); toggle(); }
      else if (k === "arrowleft") { handled(); seekBy(-5); }
      else if (k === "arrowright") { handled(); seekBy(5); }
      else if (k === "j") { handled(); seekBy(-10); }
      else if (k === "l") { handled(); seekBy(10); }
      else if (k === "arrowup") { handled(); setVol(volume + 0.1); }
      else if (k === "arrowdown") { handled(); setVol(volume - 0.1); }
      else if (k === "m") { handled(); setMuted((m) => !m); }
      else if (k === "f") { handled(); goFull(); }
      else if (k === ",") { handled(); el.pause(); el.currentTime = Math.max(0, el.currentTime - 1 / 30); }
      else if (k === ".") { handled(); el.pause(); el.currentTime = Math.min(el.duration || 0, el.currentTime + 1 / 30); }
      else if (/^[0-9]$/.test(k)) { handled(); el.currentTime = (Number(k) / 10) * (el.duration || 0); poke(); }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  const timeAt = (clientX: number) => {
    const r = bar.current?.getBoundingClientRect();
    if (!r || !duration) return 0;
    return Math.max(0, Math.min(1, (clientX - r.left) / r.width)) * duration;
  };

  // Scrubbing follows the pointer even outside the bar
  useEffect(() => {
    if (!scrubbing) return;
    const move = (e: PointerEvent) => { const el = v(); if (el) el.currentTime = timeAt(e.clientX); };
    const up = () => setScrubbing(false);
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    return () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); };
  });

  const progress = duration ? (time / duration) * 100 : 0;
  const VolIcon = muted || volume === 0 ? VolumeXIcon : volume < 0.5 ? Volume1Icon : Volume2Icon;
  const btn = "flex size-8 items-center justify-center rounded-md text-white/85 transition-colors hover:bg-white/10 hover:text-white [&_svg]:size-[18px]";

  return (
    <div ref={box} onMouseMove={poke} onMouseLeave={() => !v()?.paused && setShowUi(false)}
      className={cn("group/player relative flex items-center justify-center overflow-hidden rounded-lg bg-black select-none", !showUi && playing && "cursor-none", className)}>
      <video
        ref={video} key={src} src={src} autoPlay={autoPlay} playsInline
        className="max-h-full max-w-full"
        onClick={toggle} onDoubleClick={goFull}
        onPlay={() => { setPlaying(true); poke(); }} onPause={() => { setPlaying(false); setShowUi(true); }}
        onTimeUpdate={(e) => setTime(e.currentTarget.currentTime)}
        onLoadedMetadata={(e) => { setDuration(e.currentTarget.duration); e.currentTarget.volume = volume; e.currentTarget.muted = muted; e.currentTarget.loop = loop; }}
        onDurationChange={(e) => setDuration(e.currentTarget.duration)}
        onProgress={(e) => { const b = e.currentTarget.buffered; if (b.length && duration) setBuffered((b.end(b.length - 1) / duration) * 100); }}
        onRateChange={(e) => setRate(e.currentTarget.playbackRate)}
        onError={onError}
      />

      {/* Center state flash (play / pause / seek / volume) */}
      {flash && (
        <div className="pointer-events-none absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 rounded-full bg-black/60 px-4 py-2 text-sm font-medium text-white tabular-nums">{flash}</div>
      )}
      {!playing && (
        <button onClick={toggle} className="absolute top-1/2 left-1/2 flex size-16 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-full bg-black/55 text-white backdrop-blur transition-transform hover:scale-105">
          <PlayIcon className="ml-1 size-7 fill-current" />
        </button>
      )}

      {/* Controls */}
      <div className={cn("absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/85 via-black/45 to-transparent px-3 pt-10 pb-2 transition-opacity duration-200",
        showUi || !playing ? "opacity-100" : "pointer-events-none opacity-0")}>
        {/* Seek bar */}
        <div ref={bar} className="group/bar relative flex h-4 cursor-pointer items-center"
          onPointerDown={(e) => { const el = v(); if (el) el.currentTime = timeAt(e.clientX); setScrubbing(true); }}
          onPointerMove={(e) => setHover({ x: e.clientX - (bar.current?.getBoundingClientRect().left ?? 0), t: timeAt(e.clientX) })}
          onPointerLeave={() => setHover(null)}>
          <div className="relative h-1 w-full overflow-hidden rounded-full bg-white/20 transition-[height] group-hover/bar:h-1.5">
            <div className="absolute inset-y-0 left-0 bg-white/30" style={{ width: `${buffered}%` }} />
            <div className="absolute inset-y-0 left-0 bg-white" style={{ width: `${progress}%` }} />
          </div>
          <div className="absolute size-3 -translate-x-1/2 scale-0 rounded-full bg-white shadow transition-transform group-hover/bar:scale-100"
            style={{ left: `${progress}%` }} />
          {hover && duration > 0 && (
            <div className="pointer-events-none absolute -top-7 -translate-x-1/2 rounded bg-black/80 px-1.5 py-0.5 text-[11px] text-white tabular-nums" style={{ left: hover.x }}>
              {fmt(hover.t)}
            </div>
          )}
        </div>

        <div className="mt-1 flex items-center gap-1">
          <Tip content={playing ? "Pause (K)" : "Play (K)"}><button className={btn} onClick={toggle}>{playing ? <PauseIcon className="fill-current" /> : <PlayIcon className="fill-current" />}</button></Tip>
          <Tip content="Back 10 s (J)"><button className={btn} onClick={() => seekBy(-10)}><RotateCcwIcon /></button></Tip>
          <Tip content="Forward 10 s (L)"><button className={btn} onClick={() => seekBy(10)}><RotateCwIcon /></button></Tip>

          <div className="group/vol flex items-center">
            <Tip content={muted ? "Unmute (M)" : "Mute (M)"}><button className={btn} onClick={() => setMuted((m) => !m)}><VolIcon /></button></Tip>
            <input type="range" min={0} max={1} step={0.01} value={muted ? 0 : volume}
              onChange={(e) => { const n = Number(e.target.value); setVolume(n); setMuted(n === 0); }}
              className="h-1 w-0 cursor-pointer accent-white opacity-0 transition-all duration-200 group-hover/vol:ml-1 group-hover/vol:w-20 group-hover/vol:opacity-100" />
          </div>

          <span className="ml-2 text-xs text-white/80 tabular-nums">{fmt(time)} <span className="text-white/45">/ {fmt(duration)}</span></span>

          <div className="ml-auto flex items-center gap-1">
            <DropdownMenu>
              <Tip content="Speed">
                <DropdownMenuTrigger asChild>
                  <button className={cn(btn, "w-auto gap-1 px-2 text-xs font-medium")}><GaugeIcon />{rate === 1 ? "1×" : `${rate}×`}</button>
                </DropdownMenuTrigger>
              </Tip>
              <DropdownMenuContent side="top" align="end" container={box.current}>
                <DropdownMenuLabel>Playback speed</DropdownMenuLabel>
                {SPEEDS.map((s) => (
                  <DropdownMenuItem key={s} onSelect={() => { const el = v(); if (el) el.playbackRate = s; }}>
                    <span className={cn(s === rate && "font-semibold")}>{s === 1 ? "Normal" : `${s}×`}</span>
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
            <Tip content={loop ? "Loop is on" : "Loop"}>
              <button className={cn(btn, loop && "bg-white/15 text-white")} onClick={() => setLoop((l) => !l)}><RepeatIcon /></button>
            </Tip>
            {"pictureInPictureEnabled" in document && (
              <Tip content="Picture in picture">
                <button className={btn} onClick={() => { const el = v(); if (!el) return; if (document.pictureInPictureElement) document.exitPictureInPicture().catch(() => {}); else el.requestPictureInPicture().catch(() => {}); }}>
                  <PictureInPicture2Icon />
                </button>
              </Tip>
            )}
            <Tip content={full ? "Exit full screen (F)" : "Full screen (F)"}>
              <button className={btn} onClick={goFull}>{full ? <Minimize2Icon /> : <Maximize2Icon />}</button>
            </Tip>
          </div>
        </div>
      </div>
    </div>
  );
}
