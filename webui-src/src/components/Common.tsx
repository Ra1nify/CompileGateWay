import type { JSX, ReactNode } from "react";
import { Tag, Button } from "@fluentui/react-components";
import { X } from "lucide-react";

/** Centered loading indicator (used for page load / sign-in). */
export function CenteredSpinner({ label }: { label: string }): JSX.Element {
  return (
    <div className="flex flex-col items-center justify-center gap-3 min-h-[220px] w-full" role="status" aria-live="polite">
      <div className="w-8 h-8 rounded-full border-2 border-primary/20 border-t-primary animate-spin" />
      <span className="text-xs text-text-muted font-medium">{label}</span>
    </div>
  );
}

export interface DescriptionItem {
  term: string;
  detail: ReactNode;
  /** Use when detail is already nowrap-safe (e.g. IP:port). */
  nowrap?: boolean;
}

export function DescriptionList({
  items,
  className,
}: {
  items: DescriptionItem[];
  className?: string;
}): JSX.Element {
  return (
    <dl className={`grid grid-cols-1 sm:grid-cols-[max-content_minmax(0,1fr)] gap-x-6 gap-y-3.5 text-sm ${className || ""}`}>
      {items.map((item, index) => (
        <div key={`${item.term}-${index}`} className="contents">
          <dt className="text-text-muted font-medium select-none text-xs sm:text-sm">{item.term}</dt>
          <dd className={`text-text text-xs sm:text-sm ${item.nowrap ? "whitespace-nowrap" : "break-all"}`}>
            {item.detail}
          </dd>
        </div>
      ))}
    </dl>
  );
}

export function Panel({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}): JSX.Element {
  return (
    <div
      className={`bg-card border border-border rounded-xl p-5 shadow-sm max-w-3xl ${
        className || ""
      }`}
    >
      {children}
    </div>
  );
}

/**
 * Page head. Responsive: title & actions stack on mobile, horizontal on desktop.
 */
export function PageHead({
  title,
  subtitle,
  actions,
}: {
  title?: string;
  subtitle?: ReactNode;
  actions?: ReactNode;
}): JSX.Element {
  return (
    <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4 mb-6 pb-4 border-b border-border/80">
      <div className="min-w-0">
        {title ? <h1 className="text-lg sm:text-xl font-bold tracking-tight text-text truncate">{title}</h1> : null}
        {subtitle ? <p className="text-xs sm:text-sm text-text-muted mt-1 truncate">{subtitle}</p> : null}
      </div>
      {actions ? <div className="flex items-center gap-2 shrink-0 flex-wrap sm:flex-nowrap">{actions}</div> : null}
    </div>
  );
}

/** Read-only tag row on an instance card. */
export function TagRow({ tags }: { tags: string[] }): JSX.Element | null {
  if (!tags.length) {
    return null;
  }
  return (
    <div className="flex flex-wrap gap-1.5 items-center">
      {tags.map((tag, index) => (
        <Tag key={`${tag}-${index}`} size="extra-small" appearance="brand" shape="rounded">
          {tag}
        </Tag>
      ))}
    </div>
  );
}

/**
 * Removable tag. The dismiss (x) button sits at the top-right.
 */
export function EditorTag({
  value,
  dismissLabel,
  onDismiss,
}: {
  value: string;
  dismissLabel: string;
  onDismiss: () => void;
}): JSX.Element {
  return (
    <span className="relative inline-flex items-center pr-1">
      <Tag size="extra-small" appearance="brand" shape="rounded">
        {value}
      </Tag>
      <Button
        style={{
          position: "absolute",
          top: "-9px",
          right: "-9px",
          minWidth: "18px",
          width: "18px",
          height: "18px",
          padding: 0,
          borderRadius: "50%",
        }}
        appearance="subtle"
        size="small"
        icon={<X className="w-3 h-3" />}
        aria-label={`${dismissLabel}: ${value}`}
        title={`${dismissLabel}: ${value}`}
        onClick={onDismiss}
      />
    </span>
  );
}
