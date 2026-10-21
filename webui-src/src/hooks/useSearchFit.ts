import { useEffect, useRef, useState } from "react";
import { fitToPlaceholder } from "../format";

/** Size the search box to just fit its placeholder; re-measure when font / language / window changes. */
export function useSearchFit(
  placeholder: string,
  extra = 64,
  minimum = 200,
): (element: HTMLInputElement | null) => void {
  const [input, setInput] = useState<HTMLInputElement | null>(null);
  const ref = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    ref.current = input;
    if (!input) {
      return;
    }

    const fit = (): void => fitToPlaceholder(ref.current, placeholder, extra, minimum);
    fit();

    window.addEventListener("resize", fit);
    // Widths change once fonts (especially CJK) finish loading, so measure again.
    const fonts = (document as Document & { fonts?: FontFaceSet }).fonts;
    void fonts?.ready.then(fit).catch(() => undefined);

    return () => {
      window.removeEventListener("resize", fit);
    };
  }, [input, placeholder, extra, minimum]);

  return setInput;
}
