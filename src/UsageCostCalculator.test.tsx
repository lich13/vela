import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import UsageCostCalculator from "./UsageCostCalculator";
import { command } from "./bridge";
import type { InstanceState } from "./bridge-contracts";
import { setInstanceState } from "./instance";

vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);
/** The pages show this machine, or a server reached over SSH. */
const instance = (active: "local" | "fixture-server"): InstanceState => ({
  revision: `fixture-${active}`,
  active,
  machines: [
    {
      id: "fixture-server",
      name: "Fixture server",
      kind: "linux",
      sshHost: "fixture-host",
      fallbackHost: null,
      useSudo: false,
    },
  ],
  connection:
    active === "local"
      ? { status: "local", version: null, error: null, unsupported: [] }
      : { status: "connected", version: null, error: null, unsupported: [] },
});
const result = {
  rates: ["8", "40", "0.2", "10", "16"],
  costs: ["0.000016", "0.06", "0.0032", "1.76", "3.862656"],
  total: "5.685872",
};
beforeEach(() => {
  setInstanceState(instance("local"));
  vi.mocked(command).mockReset();
  vi.mocked(command).mockImplementation(async (name) => {
    if (name === "get_gateway")
      return {
        providers: [{ id: "fixture-provider", name: "Fixture provider" }],
      } as never;
    if (name === "get_usage_calculator_models")
      return [
        {
          model: "claude-fable-5-2",
          rates: ["8", "40", "0.2", "10", "16"],
        },
        {
          model: "claude-unpriced",
          rates: null,
        },
      ] as never;
    if (name === "get_usage_calculator_totals")
      return {
        attempts: 1,
        tokens: {
          input: 2,
          output: 1500,
          cacheRead: 16000,
          cacheWrite: 417416,
          cacheWrite5m: 176000,
          cacheWrite1h: 241416,
        },
      } as never;
    if (name === "calculate_usage_cost") return result as never;
    throw new Error(`Unexpected mutation: ${name}`);
  });
});

const ready = () =>
  waitFor(() =>
    expect(
      document.querySelector('datalist option[value="claude-fable-5-2"]'),
    ).not.toBeNull(),
  );
const called = (name: string) =>
  vi.mocked(command).mock.calls.filter(([n]) => n === name);

it("brings the five unit prices along with a used model that has a price", async () => {
  render(<UsageCostCalculator filter={{ client: "claude" }} close={() => {}} />);
  await ready();
  expect(screen.getByLabelText("供应商")).toHaveDisplayValue("全部供应商");
  fireEvent.change(screen.getByLabelText("模型"), {
    target: { value: "claude-fable-5-2" },
  });
  expect(screen.getByLabelText("输入单价 / 美元每百万")).toHaveValue(8);
  expect(screen.getByLabelText("输出单价 / 美元每百万")).toHaveValue(40);
  expect(screen.getByLabelText("缓存读取单价 / 美元每百万")).toHaveValue(0.2);
  expect(
    screen.getByLabelText("创建 5 分钟缓存单价 / 美元每百万"),
  ).toHaveValue(10);
  expect(
    screen.getByLabelText("创建 1 小时缓存单价 / 美元每百万"),
  ).toHaveValue(16);

  // A model without a price keeps the prices already entered.
  fireEvent.change(screen.getByLabelText("输出单价 / 美元每百万"), {
    target: { value: "24" },
  });
  fireEvent.change(screen.getByLabelText("模型"), {
    target: { value: "claude-unpriced" },
  });
  expect(screen.getByLabelText("模型")).toHaveValue("claude-unpriced");
  expect(screen.getByLabelText("输入单价 / 美元每百万")).toHaveValue(8);
  expect(screen.getByLabelText("输出单价 / 美元每百万")).toHaveValue(24);
  expect(screen.queryByLabelText(/倍率$|基准价格|Tokens$/)).toBeNull();
  expect(screen.queryByRole("button", { name: /填入/ })).toBeNull();
});

it("costs the usage of the chosen period at the prices entered", async () => {
  const user = userEvent.setup();
  render(
    <UsageCostCalculator
      filter={{
        start: new Date(2026, 0, 1, 0, 0, 0).getTime(),
        end: new Date(2026, 0, 1, 23, 59, 59).getTime(),
        client: "claude",
        operation: "fixture-operation",
      }}
      close={() => {}}
    />,
  );
  await screen.findByRole("option", { name: "Fixture provider" });
  await ready();
  await user.selectOptions(screen.getByLabelText("供应商"), "fixture-provider");
  fireEvent.change(screen.getByLabelText("模型"), {
    target: { value: "claude-fable-5-2" },
  });
  fireEvent.change(screen.getByLabelText("开始"), {
    target: { value: "2026-01-02T03:04" },
  });
  fireEvent.change(screen.getByLabelText("结束"), {
    target: { value: "2026-01-03T06:07" },
  });
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
  expect(called("get_usage_calculator_totals")).toEqual([
    [
      "get_usage_calculator_totals",
      {
        filter: {
          start: new Date(2026, 0, 2, 3, 4, 0).getTime(),
          end: new Date(2026, 0, 3, 6, 7, 59).getTime(),
          client: "claude",
          provider: "fixture-provider",
          model: "claude-fable-5-2",
          source: "proxy",
        },
      },
    ],
  ]);
  expect(called("calculate_usage_cost")).toEqual([
    [
      "calculate_usage_cost",
      {
        input: {
          rates: ["8", "40", "0.2", "10", "16"],
          tokens: ["2", "1500", "16000", "176000", "241416"],
        },
      },
    ],
  ]);
  const cells = [...document.querySelectorAll("tbody tr")].map((row) =>
    [...row.children].map((cell) => cell.textContent),
  );
  expect(cells[4]).toEqual(["创建 1 小时缓存", "241,416", "16", "3.862656"]);
  expect(
    [...document.querySelectorAll("thead th")].map((cell) => cell.textContent),
  ).toEqual(["项目", "Tokens", "美元 / 百万", "费用 / 美元"]);

  // Any edit drops the result it no longer describes.
  fireEvent.change(screen.getByLabelText("输入单价 / 美元每百万"), {
    target: { value: "4" },
  });
  expect(screen.queryByText("$5.685872")).not.toBeInTheDocument();
  expect(
    vi
      .mocked(command)
      .mock.calls.every(([name]) =>
        [
          "get_gateway",
          "get_usage_calculator_models",
          "get_usage_calculator_totals",
          "calculate_usage_cost",
        ].includes(name),
      ),
  ).toBe(true);
});

it("defaults to today for every supplier and prices untimed cache writes at 5 minutes", async () => {
  const user = userEvent.setup();
  vi.mocked(command).mockImplementation(async (name) => {
    if (name === "get_usage_calculator_totals")
      return {
        attempts: 2,
        tokens: { input: 7, cacheWrite: 10, cacheWrite5m: 3, cacheWrite1h: 5 },
      } as never;
    if (name === "calculate_usage_cost") return result as never;
    return (name === "get_gateway" ? { providers: [] } : []) as never;
  });
  const before = Math.floor(Date.now() / 1000) * 1000;
  render(<UsageCostCalculator filter={{}} close={() => {}} />);
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
  const [{ filter }] = called("get_usage_calculator_totals").map(
    ([, args]) => args as { filter: Record<string, unknown> },
  );
  expect(filter).toMatchObject({ client: "claude", provider: "" });
  expect(filter.model).toBeUndefined();
  expect(filter.start).toBe(new Date(before).setHours(0, 0, 0, 0));
  expect(filter.end).toBeGreaterThanOrEqual(before);
  expect(filter.end).toBeLessThanOrEqual(Date.now());
  expect(command).toHaveBeenCalledWith("calculate_usage_cost", {
    input: {
      rates: ["2", "2", "2", "2", "2"],
      tokens: ["7", "0", "0", "5", "5"],
    },
  });
});

it("reports a period without matching gateway usage and calculates nothing", async () => {
  const user = userEvent.setup();
  vi.mocked(command).mockImplementation(async (name) =>
    name === "get_usage_calculator_totals"
      ? ({ attempts: 0, tokens: {} } as never)
      : name === "get_gateway"
        ? ({ providers: [] } as never)
        : ([] as never),
  );
  render(<UsageCostCalculator filter={{}} close={() => {}} />);
  await user.click(screen.getByRole("button", { name: "计算" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "所选周期没有符合条件的网关用量",
  );
  expect(called("calculate_usage_cost")).toEqual([]);
});

it("rejects a period that starts after it ends before asking for usage", async () => {
  const user = userEvent.setup();
  render(<UsageCostCalculator filter={{}} close={() => {}} />);
  await ready();
  fireEvent.change(screen.getByLabelText("开始"), {
    target: { value: "2026-01-03T00:00:00" },
  });
  fireEvent.change(screen.getByLabelText("结束"), {
    target: { value: "2026-01-02T00:00:00" },
  });
  await user.click(screen.getByRole("button", { name: "计算" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "开始时间不能晚于结束时间",
  );
  expect(called("get_usage_calculator_totals")).toEqual([]);

  fireEvent.change(screen.getByLabelText("结束"), {
    target: { value: "2026-01-04T00:00:00" },
  });
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
  expect(screen.queryByRole("alert")).toBeNull();
});

it("keeps the inputs after a calculation failure and permits retry", async () => {
  const user = userEvent.setup();
  render(<UsageCostCalculator filter={{}} close={() => {}} />);
  await ready();
  fireEvent.change(screen.getByLabelText("输入单价 / 美元每百万"), {
    target: { value: "3" },
  });
  const usual = vi.mocked(command).getMockImplementation()!;
  vi.mocked(command).mockImplementation(async (name, ...rest) => {
    if (name === "calculate_usage_cost")
      throw new Error("fixture calculation failed");
    return usual(name, ...rest) as never;
  });
  await user.click(screen.getByRole("button", { name: "计算" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "fixture calculation failed",
  );
  expect(screen.getByLabelText("输入单价 / 美元每百万")).toHaveValue(3);
  vi.mocked(command).mockImplementation(usual);
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
});

const claudeKey = {
  id: "b7d4e9f0-2a6c-4b31-9e75-4c0a8d5e6f03",
  name: "Fixture CI",
};
const codexKey = {
  id: "3f6a1c2e-7b4d-4e19-8a50-1d2c9e6b7f01",
  name: "Fixture bot",
};
const agyKey = {
  id: "5a0c7e21-9d3f-4b86-a1e2-8f6b4c0d7e05",
  name: "Fixture agy",
};
/**
 * Claude Code, Codex and Antigravity each hand a key out and Grok Build hands
 * out none. The DeepSeek Harness gateway cannot be read.
 */
function handOutKeys() {
  const usual = vi.mocked(command).getMockImplementation()!;
  vi.mocked(command).mockImplementation(async (name, ...rest) => {
    if (name !== "get_gateway") return usual(name, ...rest) as never;
    const { clientId } = rest[0] as { clientId: string };
    if (clientId === "dsh") throw new Error("fixture gateway unreadable");
    return {
      providers: [],
      entryKeys:
        clientId === "claude"
          ? [claudeKey]
          : clientId === "codex"
            ? [codexKey]
            : clientId === "agy"
              ? [agyKey]
              : [],
    } as never;
  });
}
/** Calculates, and answers with the filter the usage was asked for with. */
async function counted(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
  const [, args] = called("get_usage_calculator_totals").at(-1)!;
  return (args as { filter: Record<string, unknown> }).filter;
}

it("counts the usage that came in with the key chosen, and lets the key go with the client", async () => {
  handOutKeys();
  const user = userEvent.setup();
  render(
    <UsageCostCalculator
      filter={{ client: "claude", entryKey: claudeKey.id }}
      close={() => {}}
    />,
  );
  // The key the page is narrowed to, once the client's keys are known.
  await screen.findByRole("option", { name: "Fixture CI" });
  const key = () => screen.getByLabelText("Key");
  expect(key()).toHaveValue(claudeKey.id);
  expect(
    [...key().querySelectorAll("option")].map((option) => option.textContent),
  ).toEqual(["全部 Key", "本机 Key", "Fixture CI"]);
  // Beside the model, in the two columns the other rows have.
  expect(key().closest("div")).toBe(
    screen.getByLabelText("模型").closest("div"),
  );
  expect(key().closest("div")).toHaveClass("usage-price-fields", {
    exact: true,
  });
  expect(await counted(user)).toMatchObject({
    client: "claude",
    source: "proxy",
    entryKey: claudeKey.id,
  });

  await user.selectOptions(key(), "本机 Key");
  expect((await counted(user)).entryKey).toBe("default");
  await user.selectOptions(key(), "全部 Key");
  expect((await counted(user)).entryKey).toBeUndefined();

  // Another client has a key of its own too, and keys it hands out: none of
  // them is chosen.
  await user.selectOptions(key(), "本机 Key");
  await user.selectOptions(screen.getByLabelText("客户端"), "codex");
  await screen.findByRole("option", { name: "Fixture bot" });
  expect(key()).toHaveValue("");
  const other = await counted(user);
  expect(other.client).toBe("codex");
  expect(other.entryKey).toBeUndefined();
  await user.selectOptions(key(), "Fixture bot");
  expect((await counted(user)).entryKey).toBe(codexKey.id);

  // A gateway that cannot be read offers no key, and not the last client's.
  await user.selectOptions(screen.getByLabelText("客户端"), "dsh");
  await ready();
  expect(screen.queryByLabelText("Key")).toBeNull();
  expect((await counted(user)).entryKey).toBeUndefined();

  // A client that hands out none shows the form as it was.
  await user.selectOptions(screen.getByLabelText("客户端"), "codex");
  await screen.findByRole("option", { name: "Fixture bot" });
  await user.selectOptions(screen.getByLabelText("客户端"), "grok");
  await ready();
  expect(screen.queryByLabelText("Key")).toBeNull();
  expect(screen.getByLabelText("模型").closest("div")).not.toHaveClass(
    "usage-price-fields",
  );
  const none = await counted(user);
  expect(none.client).toBe("grok");
  expect(none.entryKey).toBeUndefined();
});

it("counts every key where the page's key is not one the client shown hands out", async () => {
  handOutKeys();
  const user = userEvent.setup();
  // Every client is on the page, narrowed to a key of Codex, and the
  // calculator opens on Claude Code.
  const first = render(
    <UsageCostCalculator filter={{ entryKey: codexKey.id }} close={() => {}} />,
  );
  await screen.findByRole("option", { name: "Fixture CI" });
  expect(screen.getByLabelText("Key")).toHaveValue("");
  const asked = await counted(user);
  expect(asked.client).toBe("claude");
  expect(asked.entryKey).toBeUndefined();
  first.unmount();

  // The client's own key is no choice where it is the only one.
  render(
    <UsageCostCalculator
      filter={{ client: "grok", entryKey: "default" }}
      close={() => {}}
    />,
  );
  await ready();
  expect(screen.queryByLabelText("Key")).toBeNull();
  const own = await counted(user);
  expect(own.client).toBe("grok");
  expect(own.entryKey).toBeUndefined();
});

it("reads the client's keys before it counts by the key the page is narrowed to", async () => {
  handOutKeys();
  const usual = vi.mocked(command).getMockImplementation()!;
  // The gateway is slow to answer the drawer as it opens.
  let answer!: () => void;
  const slow = new Promise<void>((resolve) => {
    answer = resolve;
  });
  let asked = 0;
  vi.mocked(command).mockImplementation(async (name, ...rest) => {
    if (name === "get_gateway" && asked++ === 0) await slow;
    return usual(name, ...rest) as never;
  });
  const user = userEvent.setup();
  render(
    <UsageCostCalculator
      filter={{ client: "claude", entryKey: claudeKey.id }}
      close={() => {}}
    />,
  );
  expect(screen.queryByLabelText("Key")).toBeNull();
  // Asked again, the gateway says the key is this client's.
  expect((await counted(user)).entryKey).toBe(claudeKey.id);
  expect(called("get_usage_calculator_totals")).toHaveLength(1);
  expect(screen.getByLabelText("Key")).toHaveValue(claudeKey.id);

  // The first answer changes neither the choice nor the amount shown for it.
  answer();
  await ready();
  expect(screen.getByLabelText("Key")).toHaveValue(claudeKey.id);
  expect(screen.getByText("$5.685872")).toBeInTheDocument();
});

it("counts nothing under a key while the keys of the client shown cannot be read", async () => {
  handOutKeys();
  const usual = vi.mocked(command).getMockImplementation()!;
  let readable = false;
  vi.mocked(command).mockImplementation(async (name, ...rest) => {
    if (name === "get_gateway" && !readable)
      throw new Error("fixture gateway unreadable");
    return usual(name, ...rest) as never;
  });
  const user = userEvent.setup();
  const narrowed = render(
    <UsageCostCalculator
      filter={{ client: "claude", entryKey: claudeKey.id }}
      close={() => {}}
    />,
  );
  await ready();
  expect(screen.queryByLabelText("Key")).toBeNull();
  await user.click(screen.getByRole("button", { name: "计算" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "无法读取访问 Key，请重试",
  );
  expect(called("get_usage_calculator_totals")).toEqual([]);
  expect(called("calculate_usage_cost")).toEqual([]);

  // The key is still the one chosen once the gateway answers.
  readable = true;
  expect((await counted(user)).entryKey).toBe(claudeKey.id);
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByLabelText("Key")).toHaveValue(claudeKey.id);
  narrowed.unmount();

  // With no key chosen there is nothing to read first.
  readable = false;
  render(<UsageCostCalculator filter={{ client: "claude" }} close={() => {}} />);
  await ready();
  const every = await counted(user);
  expect(every.client).toBe("claude");
  expect(every.entryKey).toBeUndefined();
});

it("costs the Antigravity gateway's usage under the name usage knows it by", async () => {
  handOutKeys();
  const user = userEvent.setup();
  const offered = () =>
    [...screen.getByLabelText("客户端").querySelectorAll("option")].map(
      (option) => option.textContent,
    );
  const narrowed = { client: "antigravity", entryKey: agyKey.id };
  const here = render(
    <UsageCostCalculator filter={narrowed} close={() => {}} />,
  );
  await screen.findByRole("option", { name: "Fixture agy" });
  expect(offered()).toEqual([
    "Claude Code",
    "Codex",
    "Grok Build",
    "DeepSeek Harness",
    "Antigravity",
  ]);
  expect(screen.getByLabelText("客户端")).toHaveValue("antigravity");
  expect(screen.getByLabelText("Key")).toHaveValue(agyKey.id);
  // Its gateway goes by agy.
  expect(called("get_gateway")).toEqual([["get_gateway", { clientId: "agy" }]]);
  expect(called("get_usage_calculator_models")).toEqual([
    ["get_usage_calculator_models", { client: "antigravity" }],
  ]);
  expect(await counted(user)).toMatchObject({
    client: "antigravity",
    source: "proxy",
    entryKey: agyKey.id,
  });

  // Chosen by hand as well, from another client.
  await user.selectOptions(screen.getByLabelText("客户端"), "codex");
  await screen.findByRole("option", { name: "Fixture bot" });
  await user.selectOptions(screen.getByLabelText("客户端"), "antigravity");
  await user.selectOptions(await screen.findByLabelText("Key"), "Fixture agy");
  expect(await counted(user)).toMatchObject({
    client: "antigravity",
    entryKey: agyKey.id,
  });
  here.unmount();

  // A server runs no agy gateway and has none of its usage to cost.
  setInstanceState(instance("fixture-server"));
  render(<UsageCostCalculator filter={narrowed} close={() => {}} />);
  await screen.findByRole("option", { name: "Fixture CI" });
  expect(offered()).toEqual([
    "Claude Code",
    "Codex",
    "Grok Build",
    "DeepSeek Harness",
  ]);
  expect(screen.getByLabelText("客户端")).toHaveValue("claude");
  expect(screen.getByLabelText("Key")).toHaveValue("");
});
