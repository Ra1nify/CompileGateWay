import type { JSX } from "react";
import { Button, Input, makeStyles, tokens } from "@fluentui/react-components";
import { Add16Regular } from "@fluentui/react-icons";
import { useEffect, useRef, useState } from "react";
import { EditorTag } from "./Common";

const useStyles = makeStyles({
  row: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "center",
    columnGap: "10px",
    rowGap: "10px",
    paddingTop: "4px",
    paddingBottom: "4px",
  },
  inputHolder: {
    display: "inline-flex",
    alignItems: "center",
    borderRadius: tokens.borderRadiusCircular,
  },
  input: {
    minWidth: "116px",
    width: "116px",
  },
  add: {
    minWidth: "26px",
    width: "26px",
    height: "26px",
    borderRadius: tokens.borderRadiusCircular,
  },
});

/**
 * Tag editor: each tag has an x at its top-right; a trailing + adds a new tag.
 * New tags render as an inline input: Enter / blur commits, Escape cancels, empty is dropped.
 */
export function TagEditor({
  tags,
  onChange,
  addLabel,
  deleteLabel,
}: {
  tags: string[];
  onChange: (next: string[]) => void;
  addLabel: string;
  deleteLabel: string;
}): JSX.Element {
  const styles = useStyles();
  const [draft, setDraft] = useState<string | null>(null);
  const [inputElement, setInputElement] = useState<HTMLInputElement | null>(null);
  const cancelledRef = useRef(false);

  useEffect(() => {
    if (draft !== null && inputElement) {
      inputElement.focus();
    }
  }, [draft, inputElement]);

  function addTag(): void {
    setDraft("");
  }

  function commit(): void {
    if (cancelledRef.current) {
      cancelledRef.current = false;
      setDraft(null);
      return;
    }
    const value = (draft ?? "").trim();
    setDraft(null);
    if (value) {
      onChange([...tags, value]);
    }
  }

  function cancel(): void {
    cancelledRef.current = true;
    setDraft(null);
  }

  function removeAt(index: number): void {
    onChange(tags.filter((_, current) => current !== index));
  }

  return (
    <div className={styles.row}>
      {tags.map((tag, index) => (
        <EditorTag
          key={`${tag}-${index}`}
          value={tag}
          dismissLabel={deleteLabel}
          onDismiss={() => removeAt(index)}
        />
      ))}

      {draft !== null ? (
        <span className={styles.inputHolder}>
          <Input
            className={styles.input}
            input={{ ref: setInputElement }}
            size="small"
            value={draft}
            placeholder={addLabel}
            aria-label={addLabel}
            onChange={(_, data) => setDraft(data.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                commit();
              } else if (event.key === "Escape") {
                event.preventDefault();
                cancel();
              }
            }}
            onBlur={commit}
          />
        </span>
      ) : (
        <Button
          className={styles.add}
          appearance="outline"
          shape="circular"
          size="small"
          icon={<Add16Regular />}
          aria-label={addLabel}
          title={addLabel}
          onClick={addTag}
        />
      )}
    </div>
  );
}
