import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import { claudeGatewayDemo } from "./gateway-preview";
const mock = vi.hoisted(() => ({ command: vi.fn() }));
vi.mock("./bridge", () => ({ command: mock.command }));
vi.mock("./confirmation", () => ({ confirmAction: vi.fn(async () => true) }));
import ClaudeModelMapping from "./ClaudeModelMapping";
let catalog: { models: string[]; error: string | null; stale: boolean };
beforeEach(() => {
  catalog = {
    models: ["upstream-opus", "upstream-sonnet"],
    error: null,
    stale: false,
  };
  mock.command.mockReset().mockImplementation(async (name) =>
    name === "get_gateway_models"
      ? { models: [{ id: "claude-opus" }, { id: "claude-sonnet" }] }
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
  // Request models are suggested from the gateway's own list and the provider's.
  const suggestions = document.getElementById(
    requested(1).getAttribute("list")!,
  )!;
  expect(
    [...suggestions.querySelectorAll("option")].map((option) => option.value),
  ).toEqual(["claude-opus", "claude-sonnet", "upstream-opus", "upstream-sonnet"]);
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
  expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
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
