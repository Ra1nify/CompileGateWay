import type { JSX, ReactNode } from "react";
import {
  Button,
  Spinner,
  Tag,
  makeStyles,
  mergeClasses,
  shorthands,
  tokens,
} from "@fluentui/react-components";
import { Dismiss12Regular } from "@fluentui/react-icons";

/** Centered loading indicator (used for page load / sign-in). */
const useSpinnerStyles = makeStyles({
  wrap: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    justifyContent: "center",
    gap: "12px",
    minHeight: "220px",
    width: "100%",
  },
  label: {
    color: tokens.colorNeutralForeground3,
    fontSize: tokens.fontSizeBase200,
  },
});

export function CenteredSpinner({ label }: { label: string }): JSX.Element {
  const styles = useSpinnerStyles();
  return (
    <div className={styles.wrap} role="status" aria-live="polite">
      <Spinner size="medium" labelPosition="below" label={label} />
    </div>
  );
}

/** Definition list: equivalent of the old dl.status (term column + value column). */
const useDlStyles = makeStyles({
  list: {
    display: "grid",
    gridTemplateColumns: "max-content minmax(0, 1fr)",
    columnGap: "18px",
    rowGap: "8px",
    margin: 0,
    fontSize: tokens.fontSizeBase300,
  },
  term: {
    color: tokens.colorNeutralForeground3,
    whiteSpace: "nowrap",
  },
  detail: {
    margin: 0,
    overflowWrap: "anywhere",
    color: tokens.colorNeutralForeground1,
  },
});

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
  const styles = useDlStyles();
  return (
    <dl className={mergeClasses(styles.list, className)}>
      {items.map((item, index) => (
        <div key={`${item.term}-${index}`} style={{ display: "contents" }}>
          <dt className={styles.term}>{item.term}</dt>
          <dd
            className={styles.detail}
            style={item.nowrap ? { whiteSpace: "nowrap" } : undefined}
          >
            {item.detail}
          </dd>
        </div>
      ))}
    </dl>
  );
}

/** One card (old .panel / .client-card). */
const usePanelStyles = makeStyles({
  panel: {
    ...shorthands.border("1px", "solid", tokens.colorNeutralStroke2),
    ...shorthands.borderRadius("12px"),
    backgroundColor: tokens.colorNeutralBackground2,
    ...shorthands.padding("18px", "20px"),
    maxWidth: "720px",
  },
  flush: {
    maxWidth: "1080px",
    ...shorthands.padding("14px", "16px"),
  },
});

export function Panel({
  children,
  flush = false,
  className,
}: {
  children: ReactNode;
  flush?: boolean;
  className?: string;
}): JSX.Element {
  const styles = usePanelStyles();
  return (
    <section className={mergeClasses(styles.panel, flush && styles.flush, className)}>
      {children}
    </section>
  );
}

/** Page title + subtitle. */
const useHeadStyles = makeStyles({
  head: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    columnGap: "14px",
    rowGap: "6px",
    flexWrap: "nowrap",
    marginBottom: "18px",
    minWidth: 0,
  },
  titles: {
    minWidth: 0,
    overflow: "hidden",
  },
  title: {
    margin: 0,
    fontSize: tokens.fontSizeBase500,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorNeutralForeground1,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  },
  subtitle: {
    margin: "2px 0 0",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  },
  actions: {
    display: "flex",
    alignItems: "center",
    flexWrap: "nowrap",
    flex: "0 0 auto",
    columnGap: "8px",
    marginLeft: "auto",
  },
});

/**
 * Page head. Actions stay on one line (no wrap); long titles are ellipsized.
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
  const styles = useHeadStyles();
  return (
    <div className={styles.head}>
      <div className={styles.titles}>
        {title ? <h2 className={styles.title}>{title}</h2> : null}
        {subtitle ? <p className={styles.subtitle}>{subtitle}</p> : null}
      </div>
      {actions ? <div className={styles.actions}>{actions}</div> : null}
    </div>
  );
}

/** Read-only tag row on an instance card. */
const useTagStyles = makeStyles({
  tags: {
    display: "flex",
    flexWrap: "wrap",
    columnGap: "8px",
    rowGap: "8px",
    alignItems: "center",
  },
});

export function TagRow({ tags }: { tags: string[] }): JSX.Element | null {
  const styles = useTagStyles();
  if (!tags.length) {
    return null;
  }
  return (
    <div className={styles.tags}>
      {tags.map((tag, index) => (
        <Tag key={`${tag}-${index}`} size="extra-small" appearance="brand" shape="rounded">
          {tag}
        </Tag>
      ))}
    </div>
  );
}

/**
 * Removable tag. The dismiss (x) button sits at the top-right (old .tag .tag-x).
 */
const useEditorTagStyles = makeStyles({
  holder: {
    position: "relative",
    display: "inline-flex",
    alignItems: "center",
    paddingRight: "4px",
  },
  dismiss: {
    position: "absolute",
    top: "-9px",
    right: "-9px",
    minWidth: "18px",
    width: "18px",
    height: "18px",
    ...shorthands.padding(0),
    ...shorthands.borderRadius("50%"),
    backgroundColor: tokens.colorNeutralBackground1,
    ...shorthands.border("1px", "solid", tokens.colorNeutralStroke1),
    color: tokens.colorNeutralForeground3,
    ":hover": {
      color: tokens.colorPaletteRedForeground1,
      ...shorthands.borderColor(tokens.colorPaletteRedBorder2),
      backgroundColor: tokens.colorNeutralBackground1,
    },
  },
});

export function EditorTag({
  value,
  dismissLabel,
  onDismiss,
}: {
  value: string;
  dismissLabel: string;
  onDismiss: () => void;
}): JSX.Element {
  const styles = useEditorTagStyles();
  return (
    <span className={styles.holder}>
      <Tag size="extra-small" appearance="brand" shape="rounded">
        {value}
      </Tag>
      <Button
        className={styles.dismiss}
        appearance="subtle"
        size="small"
        icon={<Dismiss12Regular />}
        aria-label={`${dismissLabel}: ${value}`}
        title={`${dismissLabel}: ${value}`}
        onClick={onDismiss}
      />
    </span>
  );
}
