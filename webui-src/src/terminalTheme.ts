/**
 * **Fixed** colors for the terminal-look panel: the log panel is a terminal, so it stays dark-bg /
 * light-text like the CLI window (`--bg/--fg/--dim` in `web/style.css`). Fluent's `...Inverted`
 * tokens would flip to white-on-black under `webDarkTheme`, hence the hard-coded values below.
 */
export const TERMINAL = {
  /** Panel background (same as the CLI window's --bg). */
  background: "#12131a",
  /** Secondary background (for headers / toolbars). */
  backgroundSoft: "#171922",
  foreground: "#d6d8e0",
  /** Timestamps and secondary notes. */
  dim: "#6b7080",
  /** One step darker (empty-state hints). */
  dimmer: "#4a4f5e",
  border: "#23262f",
  accent: "#6ea8fe",
  ok: "#6fcf97",
  warn: "#e3b341",
  error: "#f2727f",
} as const;
