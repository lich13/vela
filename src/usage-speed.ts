import { useEffect, useState } from "react";
import { command } from "./bridge";
import { useInstance } from "./instance";
import type { UsageRate, UsageSpeed } from "./usage-types";

/** What a dial reads of something that is spending nothing. */
export const idleRate: UsageRate = {
  costPerHour: "0",
  level: 0,
  unpriced: false,
};

/** The colour each client's dial is lit in, by the name usage knows it by. */
export const speedColors: Record<string, string> = {
  codex: "var(--accent)",
  claude: "var(--accent-blue)",
  antigravity: "var(--accent-violet)",
  grok: "var(--accent-amber)",
  dsh: "var(--accent-emerald)",
};

/**
 * A rate in figures: the more it is, the fewer decimals it is given, so a
 * reading stays about as wide whatever it reads.
 */
export function speedLabel(rate: UsageRate): string {
  if (rate.unpriced) return "未定价";
  const perHour = Number(rate.costPerHour);
  return `$${perHour.toFixed(perHour >= 100 ? 0 : perHour >= 10 ? 1 : 2)}/h`;
}

/**
 * How fast the instance shown is spending, read at once and then every two
 * seconds for as long as `active`. Null until it is read, where a reading
 * fails and once it is no longer asked for: a dial reads what is being spent
 * now, or is not there.
 */
export function useUsageSpeed(active: boolean): UsageSpeed | null {
  const instance = useInstance().key;
  // The reading, and the instance it was asked of.
  const [read, setRead] = useState<{
    instance: string;
    speed: UsageSpeed | null;
  } | null>(null);
  useEffect(() => {
    if (!active) return;
    let stopped = false;
    let asking = false;
    const ask = () => {
      // A reading slower than the turns is waited for, not asked for again.
      if (asking) return;
      asking = true;
      command("get_usage_speed")
        // A failure every two seconds is no news to keep telling: the dials
        // go, and come back with the next reading.
        .catch(() => null)
        .then((speed) => {
          asking = false;
          if (!stopped) setRead({ instance, speed });
        });
    };
    ask();
    const timer = setInterval(ask, 2000);
    return () => {
      stopped = true;
      clearInterval(timer);
      setRead(null);
    };
  }, [active, instance]);
  // What was read while it was asked for and of this instance: the render
  // that stops the asking already shows nothing.
  return active && read?.instance === instance ? read.speed : null;
}
