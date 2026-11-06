import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function formatBytes(n: number | undefined | null): string {
  if (!n || n <= 0) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i === 0 ? 0 : 1)} ${u[i]}`;
}

/** A 0–1 fraction (queue rows) as a 0–100 percentage. */
export function pct(v: number | undefined | null): number {
  const n = Number(v) || 0;
  return Math.max(0, Math.min(100, n * 100));
}

/** A value that's already a 0–100 percentage (overall progress), kept in range. */
export function percent(v: number | undefined | null): number {
  const n = Number(v) || 0;
  return Math.max(0, Math.min(100, n));
}
