/** Human-readable byte size (base 1024, one decimal). */
export function formatBytes(bytes: number): string {
  const value = Number.isFinite(bytes) && bytes > 0 ? bytes : 0;
  if (value < 1024) {
    return `${value} B`;
  }
  const units = ["KB", "MB", "GB", "TB", "PB"];
  let scaled = value / 1024;
  let unit = 0;
  while (scaled >= 1024 && unit < units.length - 1) {
    scaled /= 1024;
    unit += 1;
  }
  const rounded = scaled >= 100 ? scaled.toFixed(0) : scaled.toFixed(1);
  return `${rounded} ${units[unit]}`;
}

/** Timestamp (ms) -> local HH:MM:SS. */
export function formatClock(tsMs: number): string {
  const date = new Date(tsMs || 0);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** Timestamp (ms) -> local YYYY-MM-DD HH:MM:SS; null for 0 / invalid input. */
export function formatDateTime(tsMs: number): string | null {
  if (!tsMs || !Number.isFinite(tsMs)) {
    return null;
  }
  const date = new Date(tsMs);
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  );
}

/** Match name / IP / port / tags with regex; bad regex syntax falls back to case-insensitive contains. */
export function matchesQuery(
  client: {
    name: string;
    host: string;
    port: number;
    tags?: readonly string[] | undefined;
  },
  rawQuery: string,
): boolean {
  const raw = rawQuery.trim();
  if (!raw) {
    return true;
  }
  const haystack = [client.name, client.host, String(client.port), ...(client.tags ?? [])].join("\n");
  let regexp: RegExp | null = null;
  try {
    regexp = new RegExp(raw, "i");
  } catch {
    regexp = null;
  }
  return regexp ? regexp.test(haystack) : haystack.toLowerCase().includes(raw.toLowerCase());
}

/** Measure real text width (more accurate than the size attribute, correct for CJK too);
 * the hidden element is created once and reused for every measurement. */
let measureEl: HTMLSpanElement | null = null;

export function measureText(text: string, font: string): number {
  if (typeof document === "undefined") {
    return 0;
  }
  if (!measureEl) {
    measureEl = document.createElement("span");
    measureEl.setAttribute("aria-hidden", "true");
    measureEl.style.cssText =
      "position:absolute;left:-9999px;top:-9999px;white-space:pre;visibility:hidden";
    document.body.append(measureEl);
  }
  measureEl.style.font = font;
  measureEl.textContent = text;
  return measureEl.getBoundingClientRect().width;
}

/** Size the control to just fit its placeholder (equivalent of the old fitSearchInput). */
export function fitToPlaceholder(
  element: HTMLElement | null,
  placeholder: string,
  extra = 56,
  minimum = 200,
): void {
  if (!element || typeof window === "undefined") {
    return;
  }
  const style = window.getComputedStyle(element);
  const font = `${style.fontStyle} ${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
  const width = Math.ceil(measureText(placeholder, font)) + extra;
  element.style.width = `${Math.max(width, minimum)}px`;
}
