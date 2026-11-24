import { useEffect } from "react";

/**
 * Tells the owner whether this draft holds unsaved changes, and that it holds
 * none once the draft is gone.
 */
export function useDirtyReport(
  dirty: boolean,
  report?: (dirty: boolean) => void,
) {
  useEffect(() => {
    report?.(dirty);
  }, [dirty, report]);
  useEffect(() => () => report?.(false), [report]);
}

/** Tells the owner whether this form is in the middle of an operation. */
export function useBusyReport(busy: boolean, report?: (busy: boolean) => void) {
  useEffect(() => {
    report?.(busy);
  }, [busy, report]);
}
