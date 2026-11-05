import { useEffect, useRef, useState } from "react";
import { ClipboardIcon, CopyIcon, ScissorsIcon, TextSelectIcon } from "lucide-react";
import { clipboard } from "@/lib/native";
import { cn } from "@/lib/utils";

type Editable = HTMLInputElement | HTMLTextAreaElement;
type MenuState = { x: number; y: number; target: Editable | null; text: string; region: HTMLElement | null } | null;

const TEXT_INPUTS = new Set(["text", "search", "url", "email", "password", "number", "tel", ""]);

function editableOf(el: EventTarget | null): Editable | null {
  if (el instanceof HTMLTextAreaElement) return el;
  if (el instanceof HTMLInputElement && TEXT_INPUTS.has(el.type)) return el;
  return null;
}

function selectionIn(el: Editable) {
  const start = el.selectionStart ?? 0;
  const end = el.selectionEnd ?? 0;
  return el.value.slice(start, end);
}

const isMac = navigator.platform.toLowerCase().includes("mac");
const MOD = isMac ? "⌘" : "Ctrl+";

/**
 * Makes the webview feel like a native app:
 *  - the browser's own right-click menu (Back, Reload, Inspect…) never shows; text fields and
 *    selectable text get a small Cut / Copy / Paste / Select all menu instead
 *  - browser shortcuts that make no sense here (print, find, history navigation, caret browsing)
 *    are switched off; reload stays available while developing
 *  - images and links can't be dragged out as if the app were a web page
 */
export function AppGuards() {
  const [menu, setMenu] = useState<MenuState>(null);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onContext = (e: MouseEvent) => {
      e.preventDefault();
      const target = editableOf(e.target);
      const region = (e.target as HTMLElement | null)?.closest?.("[data-selectable]") as HTMLElement | null;
      const selected = target ? selectionIn(target) : (window.getSelection()?.toString() ?? "");
      if (!target && !region) { setMenu(null); return; }
      setMenu({ x: e.clientX, y: e.clientY, target, text: selected, region });
    };

    const onKey = (e: KeyboardEvent) => {
      const k = e.key.toLowerCase();
      const mod = e.ctrlKey || e.metaKey;
      const blocked =
        (mod && ["p", "f", "g", "u", "j", "h", "s", "o", "n", "t", "w"].includes(k) && !e.shiftKey) ||
        (mod && e.shiftKey && ["n", "t", "delete", "b", "o"].includes(k)) ||
        ["f3", "f7", "browserback", "browserforward"].includes(k) ||
        (e.altKey && (k === "arrowleft" || k === "arrowright" || k === "home")) ||
        (import.meta.env.PROD && (k === "f5" || (mod && k === "r") || k === "f12" || (mod && e.shiftKey && ["i", "j", "c"].includes(k))));
      if (blocked) { e.preventDefault(); e.stopPropagation(); }
      if (k === "escape") setMenu(null);
    };

    // Mouse back / forward buttons would navigate the webview's history
    const onMouse = (e: MouseEvent) => { if (e.button === 3 || e.button === 4) e.preventDefault(); };
    const onDrag = (e: DragEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "IMG" || t.tagName === "A") && !t.closest("[data-draggable]")) e.preventDefault();
    };
    const close = () => setMenu(null);

    window.addEventListener("contextmenu", onContext, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("mousedown", onMouse, true);
    window.addEventListener("mouseup", onMouse, true);
    window.addEventListener("dragstart", onDrag, true);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("contextmenu", onContext, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("mousedown", onMouse, true);
      window.removeEventListener("mouseup", onMouse, true);
      window.removeEventListener("dragstart", onDrag, true);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
    };
  }, []);

  // Close when clicking anywhere else or scrolling
  useEffect(() => {
    if (!menu) return;
    const onDown = (e: MouseEvent) => { if (!ref.current?.contains(e.target as Node)) setMenu(null); };
    const onScroll = () => setMenu(null);
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("scroll", onScroll, true);
    return () => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("scroll", onScroll, true);
    };
  }, [menu]);

  // Keep the menu inside the window
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  useEffect(() => {
    if (!menu || !ref.current) { setPos(null); return; }
    const r = ref.current.getBoundingClientRect();
    setPos({
      left: Math.min(menu.x, window.innerWidth - r.width - 8),
      top: Math.min(menu.y, window.innerHeight - r.height - 8),
    });
  }, [menu]);

  if (!menu) return null;
  const el = menu.target;
  const editable = !!el && !el.readOnly && !el.disabled;
  const hasSel = menu.text.length > 0;

  const run = async (action: "cut" | "copy" | "paste" | "all") => {
    setMenu(null);
    if (action === "all") {
      if (el) { el.focus(); el.select(); }
      else if (menu.region) {
        const range = document.createRange();
        range.selectNodeContents(menu.region);
        const sel = window.getSelection();
        sel?.removeAllRanges();
        sel?.addRange(range);
      }
      return;
    }
    if (action === "copy" || action === "cut") {
      if (!hasSel) return;
      await clipboard.write(menu.text);
      if (action === "cut" && el && editable) { el.focus(); document.execCommand("delete"); }
      return;
    }
    if (action === "paste" && el && editable) {
      const text = await clipboard.read();
      if (!text) return;
      el.focus();
      // insertText keeps undo history and fires the input event React listens to
      if (!document.execCommand("insertText", false, text)) {
        const start = el.selectionStart ?? el.value.length;
        el.setRangeText(text, start, el.selectionEnd ?? start, "end");
        el.dispatchEvent(new Event("input", { bubbles: true }));
      }
    }
  };

  const Item = ({ icon: Icon, label, keys, disabled, onClick }: {
    icon: typeof CopyIcon; label: string; keys: string; disabled?: boolean; onClick: () => void;
  }) => (
    <button disabled={disabled} onMouseDown={(e) => e.preventDefault()} onClick={onClick}
      className="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-sm outline-none hover:bg-accent disabled:pointer-events-none disabled:opacity-40 [&_svg]:size-4 [&_svg]:text-muted-foreground">
      <Icon />{label}<span className="ml-auto pl-6 text-xs tracking-wide text-muted-foreground">{keys}</span>
    </button>
  );

  return (
    <div ref={ref} role="menu"
      className={cn("fixed z-[100] w-52 rounded-md border bg-popover p-1 text-popover-foreground shadow-lg", !pos && "invisible")}
      style={{ left: pos?.left ?? menu.x, top: pos?.top ?? menu.y }}>
      {el && <Item icon={ScissorsIcon} label="Cut" keys={`${MOD}X`} disabled={!editable || !hasSel} onClick={() => run("cut")} />}
      <Item icon={CopyIcon} label="Copy" keys={`${MOD}C`} disabled={!hasSel} onClick={() => run("copy")} />
      {el && <Item icon={ClipboardIcon} label="Paste" keys={`${MOD}V`} disabled={!editable} onClick={() => run("paste")} />}
      <div className="-mx-1 my-1 h-px bg-border" />
      <Item icon={TextSelectIcon} label="Select all" keys={`${MOD}A`} onClick={() => run("all")} />
    </div>
  );
}
