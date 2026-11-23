import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import { claudeGatewayDemo } from "./gateway-preview";
const mock = vi.hoisted(() => ({ command: vi.fn() }));
vi.mock("./bridge", () => ({ command: mock.command }));
vi.mock("./confirmation", () => ({ confirmAction: vi.fn(async () => true) }));
import ClaudeModelMapping, { modelFamily } from "./ClaudeModelMapping";
let catalog: { models: string[]; error: string | null; stale: boolean };
let asked: { model: string; count: number }[];
beforeEach(() => {
  asked = [
    { model: "claude-opus-4-8", count: 12 },
    { model: "claude-opus-4-8[1m]", count: 3 },
    { model: "claude-haiku-4-5-20251001", count: 2 },
    { model: "gpt-fixture", count: 1 },
  ];
  catalog = {
    models: ["upstream-opus", "upstream-sonnet"],
    error: null,
    stale: false,
  };
  mock.command.mockReset().mockImplementation(async (name) =>
    name === "get_gateway_models"
      ? { models: [{ id: "claude-opus" }, { id: "claude-sonnet" }] }
      : name === "get_claude_requested_models"
        ? asked
        : catalog,
  );
});
function mount(modelMap: Record<string, string> = {}) {
  const save = vi.fn(async () => {}),
    close = vi.fn();
  render(
    <ClaudeModelMapping
      provider={{
        ...claudeGatewayDemo.providers[0],
        id: "fixture-provider",
        modelMap,
      }}
      revision="fixture-revision"
      save={save}
      close={close}
    />,
  );
  return { save, close, user: userEvent.setup() };
}
const upstream = (row: number) =>
  screen.getByRole("combobox", { name: `映射 ${row} 上游模型` });
const requested = (row: number) =>
  screen.getByRole("combobox", { name: `映射 ${row} 请求模型` });
const family = (name: string) =>
  screen.getByRole("combobox", { name: `${name} 上游模型` });

it("judges a model's family as the gateway does", () => {
  for (const [model, expected] of [
    ["claude-fable-5-2", "fable"],
    ["claude-opus-4-1-20250805", "opus"],
    ["claude-3-opus-20240229", "opus"],
    ["claude-sonnet-4-5-20250929[1m]", "sonnet"],
    ["claude-3-5-sonnet-20241022", "sonnet"],
    ["claude-haiku-4-5-20251001", "haiku"],
    ["us.anthropic.claude-3-5-haiku-20241022-v1:0", "haiku"],
    [" Claude-Opus-4-1 ", "opus"],
    ["opus", "opus"],
    ["opusplan", "opus"],
    ["sonnet[1m]", "sonnet"],
    ["claude-opusx-4", null],
    ["claude-fixture", null],
    ["gpt-5-opus", null],
    ["notclaude-opus-4", null],
    ["", null],
  ] as const)
    expect(modelFamily(model), model).toBe(expected);
});

it("maps whole families, shows what each was asked for, and restores a saved mapping", async () => {
  const { save, user } = mount({
    "family:opus": "upstream-opus",
    "family:haiku": "retired-upstream",
    "claude-opus-4-8": "upstream-sonnet",
  });
  await waitFor(() =>
    expect(
      within(family("Sonnet")).getByRole("option", { name: "upstream-sonnet" }),
    ).toBeInTheDocument(),
  );
  expect(mock.command).toHaveBeenCalledWith("get_claude_requested_models");
  // Both sections come back from the one saved mapping.
  expect(family("Fable")).toHaveValue("");
  expect(family("Opus")).toHaveValue("upstream-opus");
  expect(family("Sonnet")).toHaveValue("");
  expect(family("Haiku")).toHaveValue("retired-upstream");
  expect(
    screen.getByRole("button", { name: "按模型 ID 单独指定（1）" }),
  ).toHaveAttribute("aria-expanded", "true");
  expect(requested(1)).toHaveValue("claude-opus-4-8");
  expect(upstream(1)).toHaveValue("upstream-sonnet");
  expect(screen.queryByRole("combobox", { name: /^映射 2/ })).toBeNull();
  // Each family lists the IDs Claude Code really sent for it.
  expect(await screen.findByTestId("asked-opus")).toHaveTextContent(
    "Claude Code 近期实际请求：claude-opus-4-8 ×12、claude-opus-4-8[1m] ×3",
  );
  expect(screen.getByTestId("asked-haiku")).toHaveTextContent(
    "claude-haiku-4-5-20251001 ×2",
  );
  expect(screen.getByTestId("asked-fable")).toHaveTextContent("近 7 天未请求");
  expect(screen.getByTestId("asked-sonnet")).toHaveTextContent("近 7 天未请求");
  expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();

  await user.selectOptions(family("Sonnet"), "upstream-sonnet");
  await user.selectOptions(family("Opus"), "不映射");
  await user.selectOptions(family("Fable"), "手动填写…");
  await user.type(
    screen.getByRole("textbox", { name: "Fable 自定义模型 ID" }),
    " typed-upstream ",
  );
  await user.click(screen.getByRole("button", { name: "保存" }));
  expect(save).toHaveBeenCalledWith(
    {
      op: "modelMapProvider",
      id: "fixture-provider",
      mapping: {
        "family:fable": "typed-upstream",
        "family:sonnet": "upstream-sonnet",
        "family:haiku": "retired-upstream",
        "claude-opus-4-8": "upstream-sonnet",
      },
    },
    "fixture-revision",
  );
});

it("keeps the per-model section folded until it is asked for", async () => {
  const { save, user } = mount({ "family:opus": "upstream-opus" });
  const toggle = screen.getByRole("button", { name: "按模型 ID 单独指定" });
  expect(toggle).toHaveAttribute("aria-expanded", "false");
  expect(screen.queryByText("请求模型")).toBeNull();
  // Adding a mapping unfolds it.
  await user.click(screen.getByRole("button", { name: "添加映射" }));
  expect(toggle).toHaveAttribute("aria-expanded", "true");
  await user.type(requested(1), "family:opus");
  await user.selectOptions(upstream(1), "手动填写…");
  await user.type(
    screen.getByRole("textbox", { name: "映射 1 自定义模型 ID" }),
    "typed-upstream",
  );
  await user.click(screen.getByRole("button", { name: "保存" }));
  // A family is set in its own row, never typed as a model ID.
  expect(screen.getByRole("alert")).toHaveTextContent(
    "映射 1 请输入完整模型 ID，不支持通配符",
  );
  expect(save).not.toHaveBeenCalled();
});

it("picks upstream models from the provider's list and saves the whole mapping", async () => {
  const { save, close, user } = mount({ "claude-opus": "retired-upstream" });
  await waitFor(() =>
    expect(
      within(upstream(1)).getByRole("option", { name: "upstream-opus" }),
    ).toBeInTheDocument(),
  );
  expect(mock.command).toHaveBeenCalledWith("list_provider_models", {
    clientId: "claude",
    providerId: "fixture-provider",
    force: false,
  });
  // A saved model the provider no longer lists stays selectable.
  expect(requested(1)).toHaveValue("claude-opus");
  expect(upstream(1)).toHaveValue("retired-upstream");
  // Request models are suggested from what was asked for, the gateway's own
  // list and the provider's.
  const suggestions = document.getElementById(
    requested(1).getAttribute("list")!,
  )!;
  expect(
    [...suggestions.querySelectorAll("option")].map((option) => option.value),
  ).toEqual([
    "claude-opus-4-8",
    "claude-opus-4-8[1m]",
    "claude-haiku-4-5-20251001",
    "gpt-fixture",
    "claude-opus",
    "claude-sonnet",
    "upstream-opus",
    "upstream-sonnet",
  ]);
  expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();

  await user.selectOptions(upstream(1), "upstream-opus");
  await user.click(screen.getByRole("button", { name: "添加映射" }));
  await user.type(requested(2), " claude-sonnet ");
  await user.selectOptions(upstream(2), "upstream-sonnet");
  await user.click(screen.getByRole("button", { name: "保存" }));
  expect(save).toHaveBeenCalledWith(
    {
      op: "modelMapProvider",
      id: "fixture-provider",
      mapping: {
        "claude-opus": "upstream-opus",
        "claude-sonnet": "upstream-sonnet",
      },
    },
    "fixture-revision",
  );
  await waitFor(() => expect(close).toHaveBeenCalled());
});

it("accepts a typed upstream model and clears the mapping when every row is removed", async () => {
  catalog = { models: [], error: "模型列表读取失败", stale: false };
  const { save, user } = mount({ "claude-opus": "upstream-opus" });
  expect(await screen.findByRole("status")).toHaveTextContent(
    "模型列表读取失败",
  );
  expect(upstream(1)).toHaveValue("upstream-opus");
  await user.selectOptions(upstream(1), "手动填写…");
  const custom = screen.getByRole("textbox", { name: "映射 1 自定义模型 ID" });
  await user.clear(custom);
  await user.type(custom, "typed-upstream");
  await user.click(screen.getByRole("button", { name: "保存" }));
  expect(save).toHaveBeenLastCalledWith(
    expect.objectContaining({ mapping: { "claude-opus": "typed-upstream" } }),
    "fixture-revision",
  );

  await user.click(screen.getByRole("button", { name: "删除映射 1" }));
  expect(screen.queryByRole("combobox", { name: /^映射/ })).toBeNull();
  await user.click(screen.getByRole("button", { name: "保存" }));
  expect(save).toHaveBeenLastCalledWith(
    expect.objectContaining({ op: "modelMapProvider", mapping: {} }),
    "fixture-revision",
  );
});

it("names the row that cannot be saved and keeps the draft", async () => {
  const { save, user } = mount({ "claude-opus": "upstream-opus" });
  await waitFor(() =>
    expect(
      within(upstream(1)).getByRole("option", { name: "upstream-sonnet" }),
    ).toBeInTheDocument(),
  );
  const submit = () => user.click(screen.getByRole("button", { name: "保存" }));
  await user.click(screen.getByRole("button", { name: "添加映射" }));
  await submit();
  expect(screen.getByRole("alert")).toHaveTextContent(
    "映射 2 未填写完整，请补全或删除",
  );
  await user.type(requested(2), "claude-*");
  await user.selectOptions(upstream(2), "upstream-sonnet");
  await submit();
  expect(screen.getByRole("alert")).toHaveTextContent(
    "映射 2 请输入完整模型 ID，不支持通配符",
  );
  await user.clear(requested(2));
  await user.type(requested(2), "claude-opus");
  await submit();
  expect(screen.getByRole("alert")).toHaveTextContent(
    "映射 2 与映射 1 的请求模型重复",
  );
  await user.clear(requested(2));
  await user.type(requested(2), "upstream-sonnet");
  await submit();
  expect(screen.getByRole("alert")).toHaveTextContent(
    "映射 2 的请求模型与上游模型相同，无需映射",
  );
  expect(save).not.toHaveBeenCalled();
  expect(requested(2)).toHaveValue("upstream-sonnet");
});

it("reloads the provider's models on request", async () => {
  const { user } = mount();
  await waitFor(() =>
    expect(mock.command).toHaveBeenCalledWith("list_provider_models", {
      clientId: "claude",
      providerId: "fixture-provider",
      force: false,
    }),
  );
  expect(screen.getByText("未映射的模型按原名转发")).toBeInTheDocument();
  catalog = { models: ["upstream-new"], error: null, stale: false };
  await user.click(screen.getByRole("button", { name: "刷新模型列表" }));
  expect(mock.command).toHaveBeenCalledWith("list_provider_models", {
    clientId: "claude",
    providerId: "fixture-provider",
    force: true,
  });
  await user.click(screen.getByRole("button", { name: "添加映射" }));
  expect(
    await within(upstream(1)).findByRole("option", { name: "upstream-new" }),
  ).toBeInTheDocument();
});
