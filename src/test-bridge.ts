import { vi } from "vitest";

/** What a test drives: the stubs and the listeners the page registered. */
export const mock = {
  command: vi.fn(),
  /** `local.command`: this machine whichever instance is shown. */
  local: vi.fn(),
  /** For `vi.mock("./confirmation", () => ({ confirmAction: mock.confirm }))`. */
  confirm: vi.fn(),
  listeners: new Map<string, (value?: unknown) => void>(),
};

/**
 * Stands in for `./bridge`:
 * `vi.mock("./bridge", async () => (await import("./test-bridge")).bridge)`.
 */
export const bridge = {
  preview: false,
  command: mock.command,
  local: { command: mock.local },
  subscribe: vi.fn(async (name: string, listener: (value: unknown) => void) => {
    mock.listeners.set(name, listener);
    return () => mock.listeners.delete(name);
  }),
};

/** `bridge` for pages that also read the freeze state: no client is frozen. */
export const unfrozenBridge = {
  ...bridge,
  command: (name: string, args: unknown) =>
    name === "get_gateway_freeze"
      ? Promise.resolve({
          clients: ["codex", "claude", "grok", "dsh"].map((clientId) => ({
            clientId,
            frozen: false,
          })),
        })
      : mock.command(name, args),
};
