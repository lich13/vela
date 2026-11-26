import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import type { EntryKey } from "./types";
import {
  ownEntryKey,
  type Dashboard,
  type UsageFilter,
  type UsageRecord,
} from "./usage-types";
import { mock } from "./test-bridge";
import {
  detailValue,
  serveUsage,
  usageDashboard,
  usageRecord,
} from "./test-fixtures";

vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);

import Usage from "./Usage";

const codexKey = {
  id: "3f6a1c2e-7b4d-4e19-8a50-1d2c9e6b7f01",
  name: "Fixture CI",
};
// Another client's key may be called the same.
const claudeKey = {
  id: "b7d4e9f0-2a6c-4b31-9e75-4c0a8d5e6f03",
  name: "Fixture CI",
};
const agyKey = {
  id: "5a0c7e21-9d3f-4b86-a1e2-8f6b4c0d7e05",
  name: "Fixture agy",
};
/** A key deleted since: only the usage still names it. */
const deleted = "e1a8c3b5-6d2f-4a97-8c14-7b9f0e2d4a04";

let records: UsageRecord[];
/**
 * How many requests the overview counts under each key. It counts every key
 * whichever one the filter names, as the instance does.
 */
let counted: Record<string, number>;

/** Serves the page. `handedOut` are each gateway's keys, by its client id. */
function serve(handedOut?: Record<string, EntryKey[]>) {
  serveUsage(
    () => records,
    (): Dashboard => ({
      ...usageDashboard(Object.values(counted).reduce((a, b) => a + b, 0)),
      entryKeys: Object.entries(counted).map(([id, requests]) => ({
        id,
        totals: usageDashboard(requests).totals,
      })),
    }),
    handedOut && (() => handedOut),
  );
}

/** The filters a command was asked with, in turn. */
const filters = (name: string) =>
  mock.command.mock.calls
    .filter(([called]) => called === name)
    .map(([, args]) => (args as { filter: UsageFilter }).filter);
const options = (select: HTMLElement) =>
  within(select)
    .getAllByRole("option")
    .map((option) => option.textContent);
/** The overview asked for last is on the page. */
const settled = () =>
  waitFor(() =>
    expect(screen.getByRole("button", { name: "刷新用量" })).toBeEnabled(),
  );
/** The gateways have answered and the overview is on the page. */
async function shown() {
  await screen.findByRole("option", { name: "Fixture Provider" });
  await waitFor(() =>
    expect(screen.getByText("估算费用").nextElementSibling).toHaveTextContent(
      "$0.0000",
    ),
  );
}

beforeEach(() => {
  records = [usageRecord()];
  counted = { [ownEntryKey]: 1 };
});

it("says nothing of keys until the instance hands one out or has counted one", async () => {
  // An instance from before there were keys, then one that hands none out.
  for (const handedOut of [undefined, {}]) {
    serve(handedOut);
    const page = render(<Usage />);
    await shown();
    expect(screen.queryByLabelText("用量 Key")).toBeNull();
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual([
      "请求日志",
      "供应商",
      "模型",
      "Claude Code",
      "Antigravity",
      "定价",
    ]);
    expect(screen.getByLabelText("用量客户端").parentElement).toHaveClass(
      "usage-filters",
      { exact: true },
    );
    expect(filters("get_usage_dashboard").at(-1)!.entryKey).toBeUndefined();
    expect(filters("get_usage_logs").at(-1)!.entryKey).toBeUndefined();
    page.unmount();
  }
});

it("narrows the overview and the request list to the key chosen", async () => {
  counted = { [ownEntryKey]: 2, [codexKey.id]: 1 };
  serve({ codex: [codexKey], claude: [claudeKey] });
  const user = userEvent.setup();
  render(<Usage />);
  await screen.findByRole("option", { name: "Fixture CI（Codex）" });
  const select = screen.getByLabelText("用量 Key");
  expect(select.parentElement).toHaveClass("usage-filters with-key", {
    exact: true,
  });
  // Under every client a key says whose it is.
  expect(options(select)).toEqual([
    "全部 Key",
    "本机 Key",
    "Fixture CI（Codex）",
    "Fixture CI（Claude Code）",
  ]);
  expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual([
    "请求日志",
    "供应商",
    "模型",
    "Key",
    "Claude Code",
    "Antigravity",
    "定价",
  ]);
  expect(filters("get_usage_dashboard").at(-1)!.entryKey).toBeUndefined();

  for (const key of [codexKey.id, ownEntryKey]) {
    await user.selectOptions(select, key);
    await waitFor(() => {
      expect(filters("get_usage_dashboard").at(-1)!.entryKey).toBe(key);
      expect(filters("get_usage_logs").at(-1)).toMatchObject({
        entryKey: key,
        page: 1,
      });
    });
  }

  // Another client hands out keys of its own, so the one chosen is let go.
  await user.selectOptions(screen.getByLabelText("用量客户端"), "claude");
  expect(select).toHaveValue("");
  expect(options(select)).toEqual(["全部 Key", "本机 Key", "Fixture CI"]);
  await waitFor(() => {
    for (const name of ["get_usage_dashboard", "get_usage_logs"]) {
      const asked = filters(name).at(-1)!;
      expect(asked.client).toBe("claude");
      expect(asked.entryKey).toBeUndefined();
    }
  });
});

it("lists every key in a tab of its own, whichever one the page is narrowed to", async () => {
  counted = { [ownEntryKey]: 3, [codexKey.id]: 2, [deleted]: 1 };
  serve({ codex: [codexKey] });
  const user = userEvent.setup();
  render(<Usage />);
  await screen.findByRole("option", { name: "Fixture CI（Codex）" });
  await user.selectOptions(screen.getByLabelText("用量 Key"), codexKey.id);
  await user.click(screen.getByRole("tab", { name: "Key" }));
  expect(screen.getByRole("tab", { name: "Key" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await waitFor(() =>
    expect(filters("get_usage_dashboard").at(-1)!.entryKey).toBe(codexKey.id),
  );
  await settled();
  const table = screen.getByRole("table");
  expect(within(table).getAllByRole("columnheader")[0]).toHaveTextContent(
    /^Key$/,
  );
  expect(
    within(table)
      .getAllByRole("row")
      .slice(1)
      .map((row) =>
        within(row)
          .getAllByRole("cell")
          .slice(0, 2)
          .map((cell) => cell.textContent),
      ),
  ).toEqual([
    ["本机 Key", "3"],
    ["Fixture CI（Codex）", "2"],
    ["Key e1a8c3b5", "1"],
  ]);
});

it("offers a key only the history names, and backs out of the client's own key", async () => {
  counted = { [ownEntryKey]: 2, [deleted]: 1 };
  serve({});
  const user = userEvent.setup();
  render(<Usage />);
  const select = await screen.findByLabelText("用量 Key");
  expect(options(select)).toEqual(["全部 Key", "本机 Key", "Key e1a8c3b5"]);

  // The overview goes on naming every key, so the choice stays on the page.
  await user.selectOptions(select, ownEntryKey);
  await waitFor(() =>
    expect(filters("get_usage_dashboard").at(-1)!.entryKey).toBe(ownEntryKey),
  );
  await settled();
  expect(screen.getByLabelText("用量 Key")).toHaveValue(ownEntryKey);
  await user.selectOptions(screen.getByLabelText("用量 Key"), "");
  await waitFor(() =>
    expect(filters("get_usage_dashboard").at(-1)!.entryKey).toBeUndefined(),
  );

  await user.selectOptions(screen.getByLabelText("用量 Key"), deleted);
  await user.click(screen.getByRole("tab", { name: "Key" }));
  await waitFor(() =>
    expect(filters("get_usage_dashboard").at(-1)!.entryKey).toBe(deleted),
  );
  await settled();
  // Nothing in the period came in with it any more.
  counted = { [ownEntryKey]: 2 };
  await user.click(screen.getByRole("button", { name: "刷新用量" }));
  await waitFor(() => expect(screen.queryByLabelText("用量 Key")).toBeNull());
  expect(screen.queryByRole("tab", { name: "Key" })).toBeNull();
  expect(screen.getByRole("tab", { name: "请求日志" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await waitFor(() => {
    expect(filters("get_usage_dashboard").at(-1)!.entryKey).toBeUndefined();
    expect(filters("get_usage_logs").at(-1)!.entryKey).toBeUndefined();
  });
});

it("names an Antigravity key by the gateway that hands it out", async () => {
  records = [usageRecord({ client: "antigravity", entryKey: agyKey.id })];
  counted = { [ownEntryKey]: 1, [agyKey.id]: 1 };
  // The gateway goes by agy, and its usage is counted under antigravity.
  serve({ agy: [agyKey] });
  const user = userEvent.setup();
  render(<Usage />);
  await screen.findByRole("option", { name: "Fixture agy（Antigravity）" });
  const select = screen.getByLabelText("用量 Key");
  expect(options(select)).toEqual([
    "全部 Key",
    "本机 Key",
    "Fixture agy（Antigravity）",
  ]);
  await user.selectOptions(screen.getByLabelText("用量客户端"), "antigravity");
  expect(options(select)).toEqual(["全部 Key", "本机 Key", "Fixture agy"]);
  await user.selectOptions(select, agyKey.id);
  await waitFor(() =>
    expect(filters("get_usage_logs").at(-1)).toMatchObject({
      client: "antigravity",
      entryKey: agyKey.id,
    }),
  );
  await user.click(await screen.findByRole("row", { name: /请求详情/ }));
  expect(
    detailValue(
      await screen.findByRole("dialog", { name: "请求详情" }),
      "访问 Key",
    ),
  ).toHaveTextContent(/^Fixture agy$/);
});

it("says which key a request came in with, and nothing where it came in with the client's own", async () => {
  records = [
    usageRecord({ id: "fixture-handed-out", entryKey: codexKey.id }),
    usageRecord({ id: "fixture-deleted", entryKey: deleted }),
    usageRecord({ id: "fixture-own" }),
  ];
  counted = { [ownEntryKey]: 1, [codexKey.id]: 1, [deleted]: 1 };
  serve({ codex: [codexKey] });
  const user = userEvent.setup();
  render(<Usage />);
  await screen.findByRole("option", { name: "Fixture CI（Codex）" });
  const detail = async (index: number) => {
    await user.click(
      (await screen.findAllByRole("row", { name: /请求详情/ }))[index],
    );
    return screen.findByRole("dialog", { name: "请求详情" });
  };
  const close = async (dialog: HTMLElement) => {
    await user.click(within(dialog).getByRole("button", { name: "关闭详情" }));
    expect(screen.queryByRole("dialog", { name: "请求详情" })).toBeNull();
  };

  let dialog = await detail(0);
  expect(detailValue(dialog, "请求 ID")).toHaveTextContent(
    "fixture-handed-out",
  );
  // The client is beside it, so the name stands alone.
  expect(detailValue(dialog, "访问 Key")).toHaveTextContent(/^Fixture CI$/);
  await close(dialog);

  dialog = await detail(1);
  expect(detailValue(dialog, "访问 Key")).toHaveTextContent(/^Key e1a8c3b5$/);
  await close(dialog);

  dialog = await detail(2);
  expect(detailValue(dialog, "请求 ID")).toHaveTextContent("fixture-own");
  expect(within(dialog).queryByText("访问 Key")).toBeNull();
});

it("lets the key go on the pages that do not count by one", async () => {
  counted = { [ownEntryKey]: 2, [claudeKey.id]: 1 };
  serve({ claude: [claudeKey] });
  const usual = mock.command.getMockImplementation()!;
  mock.command.mockImplementation(
    async (name: string, args?: Record<string, unknown>) =>
      name === "get_claude_usage_stats"
        ? // Still being read when the test ends.
          new Promise(() => {})
        : name === "get_claude_usage_quota"
          ? { account: null, plan: null, windows: [], error: null }
          : usual(name, args),
  );
  const user = userEvent.setup();
  render(<Usage />);
  await screen.findByRole("option", { name: "Fixture CI（Claude Code）" });
  const select = screen.getByLabelText("用量 Key");
  await user.selectOptions(select, claudeKey.id);
  expect(select).toHaveValue(claudeKey.id);

  await user.click(screen.getByRole("tab", { name: "Claude Code" }));
  expect(select).toBeDisabled();
  expect(select).toHaveValue("");
  await waitFor(() =>
    expect(filters("get_claude_usage_stats")).not.toHaveLength(0),
  );
  for (const asked of filters("get_claude_usage_stats")) {
    expect(asked.client).toBe("claude");
    expect(asked.entryKey).toBeUndefined();
  }

  // Back on the shared pages the client stays, and no key is chosen.
  await user.click(screen.getByRole("tab", { name: "请求日志" }));
  expect(select).toBeEnabled();
  expect(select).toHaveValue("");
  expect(options(select)).toEqual(["全部 Key", "本机 Key", "Fixture CI"]);
  await waitFor(() =>
    expect(filters("get_usage_logs").at(-1)).toMatchObject({
      client: "claude",
      entryKey: undefined,
    }),
  );
});

it("opens the calculator on the client whose key the page is narrowed to", async () => {
  counted = { [ownEntryKey]: 2, [codexKey.id]: 1 };
  serve({ codex: [codexKey], claude: [claudeKey] });
  const usual = mock.command.getMockImplementation()!;
  mock.command.mockImplementation(
    async (name: string, args?: Record<string, unknown>) => {
      switch (name) {
        case "get_pricing":
        case "get_models_dev":
          // Still being read when the test ends.
          return new Promise(() => {});
        case "get_usage_calculator_models":
          return [];
        case "get_usage_calculator_totals":
          return { attempts: 1, tokens: { input: 1 } };
        case "calculate_usage_cost":
          return {
            rates: ["2", "2", "2", "2", "2"],
            costs: ["0.000002", "0", "0", "0", "0"],
            total: "0.000002",
          };
        default:
          return usual(name, args);
      }
    },
  );
  const user = userEvent.setup();
  render(<Usage />);
  await screen.findByRole("option", { name: "Fixture CI（Codex）" });
  // Every client is shown, and the key is one Codex hands out.
  await user.selectOptions(screen.getByLabelText("用量 Key"), codexKey.id);
  await user.click(screen.getByRole("tab", { name: "定价" }));
  await user.click(await screen.findByRole("button", { name: "倍率换算" }));
  const drawer = within(
    await screen.findByRole("dialog", { name: "倍率换算" }),
  );
  expect(drawer.getByLabelText("客户端")).toHaveValue("codex");
  await drawer.findByRole("option", { name: "Fixture CI" });
  expect(drawer.getByLabelText("Key")).toHaveValue(codexKey.id);
  await user.click(drawer.getByRole("button", { name: "计算" }));
  await drawer.findByText("$0.000002");
  expect(filters("get_usage_calculator_totals")).toHaveLength(1);
  expect(filters("get_usage_calculator_totals")[0]).toMatchObject({
    client: "codex",
    source: "proxy",
    entryKey: codexKey.id,
  });
  // The page goes on showing every client.
  expect(screen.getByLabelText("用量客户端")).toHaveValue("");
  expect(screen.getByLabelText("用量 Key")).toHaveValue(codexKey.id);
});

it("follows the keys as their gateways change them", async () => {
  // Handed out by another window once the page had read the gateways, and
  // requests have come in with it since.
  const fresh = {
    id: "9c2e4b7a-1f5d-4c83-b6a0-3e8d7f1a2b06",
    name: "Fixture fresh",
  };
  counted = { [ownEntryKey]: 2, [codexKey.id]: 1, [fresh.id]: 1 };
  const handedOut: Record<string, EntryKey[]> = { codex: [codexKey] };
  serve(handedOut);
  const user = userEvent.setup();
  render(<Usage />);
  await screen.findByRole("option", { name: "Fixture CI（Codex）" });
  const select = screen.getByLabelText("用量 Key");
  await waitFor(() =>
    expect(options(select)).toEqual([
      "全部 Key",
      "本机 Key",
      "Fixture CI（Codex）",
      "Key 9c2e4b7a",
    ]),
  );
  const says = (clientId: string, entryKeys: EntryKey[]) =>
    act(() =>
      mock.listeners.get("gateway-state")!({
        clientId,
        providers: [],
        entryKeys,
      }),
    );

  // Its gateway says whose it is and what it is called.
  says("claude", [fresh]);
  expect(options(select)).toEqual([
    "全部 Key",
    "本机 Key",
    "Fixture CI（Codex）",
    "Fixture fresh（Claude Code）",
  ]);

  // What no event told the page of is read again when the page is refreshed.
  handedOut.codex = [{ ...codexKey, name: "Fixture renamed" }];
  handedOut.claude = [fresh];
  handedOut.agy = [agyKey];
  await user.click(screen.getByRole("button", { name: "刷新用量" }));
  await waitFor(() =>
    expect(options(select)).toEqual([
      "全部 Key",
      "本机 Key",
      "Fixture renamed（Codex）",
      "Fixture fresh（Claude Code）",
      "Fixture agy（Antigravity）",
    ]),
  );
  await settled();

  // An answer that set out before a gateway spoke for itself is behind it.
  const usual = mock.command.getMockImplementation()!;
  let answer!: () => void;
  const slow = new Promise<void>((resolve) => {
    answer = resolve;
  });
  mock.command.mockImplementation(
    async (name: string, args?: Record<string, unknown>) => {
      const value = await usual(name, args);
      if (name === "get_gateway" && args?.clientId === "codex") await slow;
      return value;
    },
  );
  await user.click(screen.getByRole("button", { name: "刷新用量" }));
  says("codex", []);
  says("agy", [{ ...agyKey, name: "Fixture latest" }]);
  const latest = [
    "全部 Key",
    "本机 Key",
    "Fixture fresh（Claude Code）",
    "Fixture latest（Antigravity）",
    // Taken back: only its usage names it now.
    "Key 3f6a1c2e",
  ];
  expect(options(select)).toEqual(latest);
  await act(async () => {
    answer();
    await new Promise((resolve) => setTimeout(resolve));
  });
  expect(options(select)).toEqual(latest);
});
