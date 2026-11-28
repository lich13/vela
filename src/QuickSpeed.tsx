import { useEffect, useState } from "react";
import { command } from "./bridge";
import { clients, quickClientName } from "./ClientSelection";
import { useInstance } from "./instance";
import Speedometer from "./Speedometer";
import { subscriptions } from "./subscriptions";
import type { ClientId } from "./types";
import { idleRate, speedColors, useUsageSpeed } from "./usage-speed";
import { useVisible } from "./visibility";

/** Usage names the Antigravity gateway `antigravity`; the panel's id is `agy`. */
const usageName = (id: ClientId) => (id === "agy" ? "antigravity" : id);

/**
 * How fast the instance is spending right now: every platform together on the
 * large dial, and each platform the panel shows on a small one. It reads only
 * while the panel is on screen, and is not there until something is read.
 */
export default function QuickSpeed() {
  const { supports } = useInstance();
  // The panel's own state reports what fails here; a dial has nothing to add.
  const visible = useVisible("panel-visibility", () => {});
  const speed = useUsageSpeed(visible && supports("get_usage_speed"));
  const [hidden, setHidden] = useState<ClientId[]>([]);
  useEffect(() => {
    let disposed = false;
    let told = false;
    const events = subscriptions(() => {});
    void events.watch("quick-state", (prefs) => {
      told = true;
      setHidden(prefs.hiddenClients ?? []);
    });
    const read = async () => {
      const prefs = await command("get_quick");
      if (!disposed && !told) setHidden(prefs.hiddenClients ?? []);
    };
    void read().catch(() => {});
    return () => {
      disposed = true;
      events.dispose();
    };
  }, []);
  if (!speed) return null;
  return (
    <section className="quick-speed" aria-label="实时速率">
      <Speedometer
        size="large"
        rate={speed}
        label="总速率"
        name="总速率"
        marks={speed.dial}
      />
      <div className="quick-speed-clients">
        {clients
          .filter((id) => !hidden.includes(id))
          .map((id) => {
            const name = quickClientName(id);
            return (
              <Speedometer
                key={id}
                rate={
                  speed.clients.find((c) => c.id === usageName(id)) ?? idleRate
                }
                label={`${name} 速率`}
                name={name}
                color={speedColors[usageName(id)]}
              />
            );
          })}
      </div>
    </section>
  );
}
