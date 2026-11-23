import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, expect, it, vi } from "vitest";
import type { Field } from "./claude-settings";
import { codexTables, type NamedTables } from "./config-fields";
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
function Host({
  shown = fields,
  tables,
}: {
  shown?: Record<string, Field[]>;
  tables?: NamedTables[];
}) {
  const [text, setText] = useState("base");
  return (
    <>
      <output data-testid="draft">{text}</output>
      <ConfigTreeSettings
        format="toml"
        text={text}
        fields={shown}
        tables={tables}
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
  // The credential, the date and the table inside a list.
  expect(screen.getAllByText("在原文中编辑")).toHaveLength(3);
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

// A provider file as Rust reports it: credentials arrive hidden.
const providers = table([
  [
    "model_providers",
    table([
      [
        "relay",
        table([
          ["name", { kind: "string", value: "Relay" }],
          ["base_url", { kind: "string", value: "https://example.invalid/v1" }],
          ["wire_api", { kind: "string", value: "responses" }],
          ["request_max_retries", { kind: "integer", value: 1 }],
          ["supports_websockets", { kind: "boolean", value: false }],
          [
            "query_params",
            table([["api-version", { kind: "string", value: "1" }]]),
          ],
          ["env_key", { kind: "secret" }],
          ["http_headers", table([["X-Fixture", { kind: "secret" }]])],
          ["future_flag", { kind: "boolean", value: true }],
        ]),
      ],
      ["spare", table([["base_url", { kind: "string", value: "" }]])],
    ]),
  ],
]);
async function named(tab: string) {
  trees = { base: providers };
  const user = userEvent.setup();
  render(<Host tables={codexTables} />);
  await user.click(await screen.findByRole("button", { name: tab }));
  return user;
}

it("具名子表逐个列出，模板字段带标签并按子表路径写回", async () => {
  const user = await named("模型供应商");
  expect(screen.getByText("relay")).toBeInTheDocument();
  expect(screen.getByText("spare")).toBeInTheDocument();
  await user.click(screen.getByText("relay"));
  expect(screen.getByLabelText("relay 接口地址")).toHaveValue(
    "https://example.invalid/v1",
  );
  expect(screen.getByRole("combobox", { name: "relay 接口协议" })).toHaveValue(
    "responses",
  );
  // A retry count of 1 is an amount, not a switch.
  fireEvent.change(
    screen.getByRole("spinbutton", { name: "relay 请求重试次数" }),
    { target: { value: "5" } },
  );
  await user.click(screen.getByRole("switch", { name: "relay 支持 WebSocket" }));
  await user.click(
    screen.getByRole("switch", { name: "relay 使用 OpenAI 登录凭据" }),
  );
  // Keys outside the template keep their raw name.
  await user.click(
    screen.getByRole("switch", { name: "model_providers.relay.future_flag" }),
  );
  await waitFor(() =>
    expect(patches()).toEqual([
      [["model_providers", "relay", "request_max_retries"], 5],
      [["model_providers", "relay", "supports_websockets"], true],
      [["model_providers", "relay", "requires_openai_auth"], true],
      [["model_providers", "relay", "future_flag"], false],
    ]),
  );
});

it("键值表逐键打补丁", async () => {
  const user = await named("模型供应商");
  await user.click(screen.getByText("relay"));
  fireEvent.change(screen.getByLabelText("relay 查询参数 api-version"), {
    target: { value: "2" },
  });
  await user.type(screen.getByLabelText("relay 查询参数 新键"), "region");
  await user.type(screen.getByLabelText("relay 查询参数 新值"), "test");
  await user.click(
    screen.getByRole("button", { name: "确认添加relay 查询参数" }),
  );
  await user.click(
    screen.getByRole("button", { name: "删除relay 查询参数 api-version" }),
  );
  await waitFor(() =>
    expect(patches()).toEqual([
      [["model_providers", "relay", "query_params", "api-version"], "2"],
      [["model_providers", "relay", "query_params", "region"], "test"],
      [["model_providers", "relay", "query_params", "api-version"], null],
    ]),
  );
});

it("子表里的凭据键只读，并提示在原文中编辑", async () => {
  const user = await named("模型供应商");
  await user.click(screen.getByText("relay"));
  const card = within(screen.getByText("relay").closest("details")!);
  const row = card.getByText("API Key 环境变量").closest(".setting-row")!;
  expect(row).toHaveTextContent("已设置");
  expect(row).toHaveTextContent("在原文中编辑");
  expect(row.querySelector("input")).toBeNull();
  expect(
    card.getByText("Bearer 令牌").closest(".setting-row"),
  ).toHaveTextContent("未设置");
  expect(screen.queryByLabelText(/X-Fixture/)).toBeNull();
  expect(
    screen.queryByRole("button", { name: /http_headers 添加字段/ }),
  ).toBeNull();
  // A credential cannot be added as a field either.
  await user.click(
    screen.getByRole("button", { name: "model_providers.relay 添加字段" }),
  );
  await user.type(
    screen.getByLabelText("model_providers.relay 字段名称"),
    "api_key",
  );
  expect(screen.getByText("凭据在原文中编辑")).toBeInTheDocument();
  expect(screen.getAllByRole("button", { name: "添加" })[0]).toBeDisabled();
  expect(patches()).toEqual([]);
});

it("新增子表写入名称下的首个字段", async () => {
  const user = await named("模型供应商");
  const add = screen.getByRole("button", { name: "添加" });
  await user.type(screen.getByLabelText("新模型供应商名称"), "relay");
  await user.type(
    screen.getByLabelText("新模型供应商接口地址"),
    "https://example.invalid/v2",
  );
  expect(add).toBeDisabled();
  await user.type(screen.getByLabelText("新模型供应商名称"), "-2");
  await user.click(add);
  await user.click(screen.getByRole("button", { name: "MCP 服务器" }));
  expect(screen.getByText("还没有MCP 服务器")).toBeInTheDocument();
  await user.type(screen.getByLabelText("新MCP 服务器名称"), "docs");
  await user.selectOptions(
    screen.getByLabelText("新MCP 服务器首个字段"),
    "url",
  );
  await user.type(
    screen.getByLabelText("新MCP 服务器服务地址"),
    "https://example.invalid/mcp",
  );
  await user.click(screen.getByRole("button", { name: "添加" }));
  await waitFor(() =>
    expect(patches()).toEqual([
      [
        ["model_providers", "relay-2", "base_url"],
        "https://example.invalid/v2",
      ],
      [["mcp_servers", "docs", "url"], "https://example.invalid/mcp"],
    ]),
  );
});

it("子表和通用树的表都能添加字段", async () => {
  const user = await named("模型供应商");
  await user.click(screen.getByText("spare"));
  await user.click(
    screen.getByRole("button", { name: "model_providers.spare 添加字段" }),
  );
  await user.type(
    screen.getByLabelText("model_providers.spare 字段名称"),
    "model_catalog_url",
  );
  await user.type(
    screen.getByLabelText("model_providers.spare 字段值"),
    "https://example.invalid/models",
  );
  await user.click(screen.getAllByRole("button", { name: "添加" })[0]);
  await user.click(screen.getByRole("button", { name: "全部设置" }));
  await user.click(screen.getByRole("button", { name: "配置 添加字段" }));
  await user.type(screen.getByLabelText("配置 字段名称"), "max_depth");
  await user.selectOptions(screen.getByLabelText("配置 字段类型"), "number");
  expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();
  await user.type(screen.getByLabelText("配置 字段值"), "3");
  await user.click(screen.getByRole("button", { name: "添加" }));
  await user.click(screen.getByRole("button", { name: "配置 添加字段" }));
  await user.type(screen.getByLabelText("配置 字段名称"), "verbose");
  await user.selectOptions(screen.getByLabelText("配置 字段类型"), "boolean");
  await user.click(screen.getByRole("button", { name: "添加" }));
  await waitFor(() =>
    expect(patches()).toEqual([
      [
        ["model_providers", "spare", "model_catalog_url"],
        "https://example.invalid/models",
      ],
      [["max_depth"], 3],
      [["verbose"], false],
    ]),
  );
});
