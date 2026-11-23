import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, expect, it, vi } from "vitest";
import type { Field } from "./claude-settings";
import type { ConfigTreeNode } from "./config-tree";
const mock = vi.hoisted(() => ({ command: vi.fn() }));
vi.mock("./bridge", () => ({ command: mock.command }));
import ConfigTreeSettings from "./ConfigTreeSettings";

// Synthetic drafts: the fixture maps each draft text to the tree Rust would read.
let trees: Record<string, ConfigTreeNode>;
const table = (entries: [string, ConfigTreeNode][]): ConfigTreeNode => ({
  kind: "table",
  entries,
});
const base = table([
  ["enabled", { kind: "boolean", value: false }],
  ["legacy", { kind: "integer", value: 1 }],
  ["quoted", { kind: "string", value: "0" }],
  ["timeout", { kind: "integer", value: 1 }],
  ["name", { kind: "string", value: "fixture" }],
  ["api_key", { kind: "secret" }],
  ["created", { kind: "other", text: "2026-01-01T00:00:00Z" }],
  ["hosts", { kind: "array", items: [{ kind: "string", value: "127.0.0.1" }] }],
  ["servers", { kind: "array", items: [table([])] }],
]);
const fields: Record<string, Field[]> = {
  功能: [
    { label: "网页搜索", path: ["features", "web_search"], kind: "boolean" },
    { label: "名称", path: ["name"], kind: "text" },
  ],
};
const changed = vi.fn();
const patches = () =>
  mock.command.mock.calls
    .filter(([name]) => name === "patch_config_tree")
    .map(([, args]) => [args.path, args.value]);
beforeEach(() => {
  changed.mockReset();
  trees = { base };
  mock.command.mockReset().mockImplementation(async (name, args) => {
    if (name === "read_config_tree") {
      if (!trees[args.text]) throw { code: "TOML", message: "第 1 行格式错误" };
      return trees[args.text];
    }
    if (name === "patch_config_tree") return `${args.text}+`;
    throw Error("unexpected fixture command");
  });
});
function Host({ shown = fields }: { shown?: Record<string, Field[]> }) {
  const [text, setText] = useState("base");
  return (
    <>
      <output data-testid="draft">{text}</output>
      <ConfigTreeSettings
        format="toml"
        text={text}
        fields={shown}
        onChange={(next) => {
          changed(next);
          setText(next);
        }}
      />
    </>
  );
}
async function everything(shown?: Record<string, Field[]>) {
  const user = userEvent.setup();
  render(<Host shown={shown} />);
  await user.click(await screen.findByRole("button", { name: "全部设置" }));
  return user;
}

it("布尔开关写回布尔值，0/1 开关沿用原来的写法", async () => {
  const user = await everything();
  await user.click(screen.getByRole("switch", { name: "enabled" }));
  await waitFor(() => expect(changed).toHaveBeenCalledWith("base+"));
  expect(patches()).toEqual([[["enabled"], true]]);
});

it("数字 0/1 写回数字，字符串 0/1 写回字符串", async () => {
  const user = await everything();
  expect(screen.getByRole("switch", { name: "legacy" })).toBeChecked();
  expect(screen.getByRole("switch", { name: "quoted" })).not.toBeChecked();
  await user.click(screen.getByRole("switch", { name: "legacy" }));
  await waitFor(() => expect(patches()).toHaveLength(1));
  // The fixture does not know the patched draft, so the first tree stays shown.
  await user.click(screen.getByRole("switch", { name: "quoted" }));
  await waitFor(() => expect(patches()).toHaveLength(2));
  expect(patches()).toEqual([
    [["legacy"], 0],
    [["quoted"], "1"],
  ]);
  // The second patch is made on the text the first one returned.
  expect(mock.command).toHaveBeenCalledWith(
    "patch_config_tree",
    expect.objectContaining({ text: "base+", path: ["quoted"] }),
  );
});

it("数量键的 1 仍是数字输入框", async () => {
  await everything();
  expect(screen.queryByRole("switch", { name: "timeout" })).toBeNull();
  const input = screen.getByRole("spinbutton", { name: "timeout" });
  fireEvent.change(input, { target: { value: "30" } });
  await waitFor(() => expect(patches()).toEqual([[["timeout"], 30]]));
});

it("精选字段在键不存在时可以设置，恢复默认会删除该键", async () => {
  const user = userEvent.setup();
  render(<Host />);
  const search = await screen.findByRole("switch", { name: "网页搜索" });
  expect(screen.getByText("默认")).toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "网页搜索恢复默认" }),
  ).toBeDisabled();
  await user.click(search);
  await waitFor(() =>
    expect(patches()).toEqual([[["features", "web_search"], true]]),
  );
  await user.click(screen.getByRole("button", { name: "名称恢复默认" }));
  await waitFor(() => expect(patches()[1]).toEqual([["name"], null]));
});

it("没有精选字段时只显示通用结构树", async () => {
  render(<Host shown={{}} />);
  expect(await screen.findByRole("switch", { name: "enabled" })).toBeVisible();
  expect(screen.queryByRole("navigation")).toBeNull();
});

it("凭据只显示已设置，特殊值和列表里的表留给原文", async () => {
  const user = await everything();
  expect(screen.getByText("已设置")).toBeInTheDocument();
  expect(screen.queryByLabelText("api_key")).toBeNull();
  expect(screen.getByText("2026-01-01T00:00:00Z")).toBeInTheDocument();
  expect(screen.getAllByText("在原文中编辑")).toHaveLength(2);
  expect(screen.getByLabelText("hosts.0")).toHaveValue("127.0.0.1");
  await user.type(screen.getByLabelText("添加 hosts"), "example.invalid");
  await user.click(screen.getByRole("button", { name: "确认添加 hosts" }));
  await user.click(screen.getByRole("button", { name: "删除 hosts.0" }));
  await waitFor(() =>
    expect(patches()).toEqual([
      [["hosts", 1], "example.invalid"],
      [["hosts", 0], null],
    ]),
  );
});

it("补丁失败时草稿不变并显示错误", async () => {
  const user = await everything();
  mock.command.mockImplementation(async (name) => {
    if (name === "read_config_tree") return base;
    throw { code: "CONFIG", message: "不能在这里修改凭据" };
  });
  await user.click(screen.getByRole("switch", { name: "enabled" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "不能在这里修改凭据",
  );
  expect(changed).not.toHaveBeenCalled();
  expect(screen.getByTestId("draft")).toHaveTextContent("base");
});

it("结构树命令报 CONFIG 时显示消息并提示改用原文", async () => {
  mock.command.mockRejectedValue({
    code: "CONFIG",
    message: "浏览器预览不解析 TOML／YAML",
  });
  render(<Host />);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "浏览器预览不解析 TOML／YAML；请切换到原文模式修正",
  );
  expect(screen.queryByRole("switch")).toBeNull();
  expect(screen.getByTestId("draft")).toHaveTextContent("base");
});

it("草稿解析失败时保留上一次的结构树", async () => {
  const user = await everything();
  await user.click(screen.getByRole("switch", { name: "enabled" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("第 1 行格式错误");
  expect(screen.getByRole("switch", { name: "enabled" })).toBeInTheDocument();
  expect(screen.getByTestId("draft")).toHaveTextContent("base+");
});
