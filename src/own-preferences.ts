import { useEffect, useState } from "react";
import { local } from "./bridge";
import { useInstance } from "./instance";
import type { Preferences } from "./types";

/**
 * The look and the notification switch are this app's own. `shown` is what
 * the instance on screen holds; with a server on screen the app's own are read
 * from this machine instead, and `keep` records them after a save there.
 */
export function useOwnPreferences(
  shown: Preferences | undefined,
  failed: (error: unknown) => void,
) {
  const { remote } = useInstance();
  const [own, keep] = useState<Preferences>();
  useEffect(() => {
    if (!remote) return;
    let gone = false;
    void local
      .command("get_state")
      .then((state) => {
        if (!gone) keep(state.preferences);
      })
      .catch((error) => {
        if (!gone) failed(error);
      });
    return () => {
      gone = true;
    };
    // Read once per instance; `failed` only reports.
  }, [remote]);
  return [remote ? own : shown, keep] as const;
}
