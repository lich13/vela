import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, it, expect, vi } from "vitest";
import type { ConfigDocument } from "./types";
const mocks = vi.hoisted(() => ({
  command: vi.fn(),
  events: new Map<string, (p: unknown) => void>(),
}));
vi.mock("./bridge", () => ({
  command: mocks.command,
  subscribe: vi.fn(async (event, fn) => {
    mocks.events.set(event, fn);
    return () => mocks.events.delete(event);
  }),
}));
vi.mock("@uiw/react-codemirror", () => ({
  default: ({
    value,
    onChange,
    "aria-label": label,
  }: {
    value: string;
    onChange: (s: string) => void;
    "aria-label": string;
  }) => (
    <textarea
      aria-label={label}
      value={value}
      onChange={(e) => onChange(e.target.value)}
    />
  ),
}));
// The CLI form belongs to another change; a stand-in shows what the editor hands it.
vi.mock("./AntigravityCliSettings", () => ({
  default: ({
    text,
    onChange,
  }: {
    text: string;
    onChange: (text: string) => void;
  }) => (
    <input
      aria-label="CLI 表单"
      value={text}
      onChange={(e) => onChange(e.target.value)}
    />
  ),
}));
import ConfigEditor from "./ConfigEditor";
import { get } from "./claude-settings";
let doc: ConfigDocument;
beforeEach(() => {
  localStorage.clear();
  localStorage.setItem("vela.config.client", "claude");
  mocks.events.clear();
  mocks.command.mockReset();
  doc = {
    clientId: "claude",
    path: "/fixture/settings.json",
    text: '{"model":"old","env":{"ANTHROPIC_MODEL":"effective","ANTHROPIC_AUTH_TOKEN":"fixture-secret"},"future":0}',
    revision: "r1",
    guarded: false,
    canRestore: true,
  };
  mocks.command.mockImplementation(async (name, args) => {
    if (name === "read_config") return { ...doc };
    if (name === "validate_config") {
      get(args.text, []);
      return;
    }
    if (name === "read_previous_config") return '{"language":"中文"}';
    if (name === "save_config") {
      get(args.text, []);
      if (args.expectedRevision !== doc.revision)
        throw { code: "CONFLICT", message: "配置已变化，草稿已保留" };
      doc = { ...doc, text: args.text, revision: "r2" };
      return { ...doc };
    }
  });
});
function mount() {
  return render(
    <ConfigEditor
      revision="codex-only"
      home="/codex"
      claudeHome="/claude"
      grokHome="/grok"
      theme="dark"
      onDirty={vi.fn()}
      onMessage={vi.fn()}
    />,
  );
}
it("shares visual/source drafts, edits the effective model, and saves once with Mod+S", async () => {
  const u = userEvent.setup();
  mount();
  const model = await screen.findByLabelText("默认模型");
  expect(model).toHaveValue("effective");
  fireEvent.change(model, { target: { value: "manual-model" } });
  await u.click(screen.getByRole("button", { name: "JSON" }));
  const source = (await screen.findByLabelText(
    "JSON 编辑器",
  )) as HTMLTextAreaElement;
  expect(JSON.parse(source.value).model).toBe("old");
  expect(JSON.parse(source.value).env.ANTHROPIC_MODEL).toBe("manual-model");
  await u.click(screen.getByRole("button", { name: "可视化" }));
  expect(screen.getByLabelText("默认模型")).toHaveValue("manual-model");
  fireEvent.keyDown(window, { key: "s", ctrlKey: true });
  await waitFor(() =>
    expect(
      mocks.command.mock.calls.filter(([n]) => n === "save_config"),
    ).toHaveLength(1),
  );
  expect(doc.text).toContain('"future":0');
  expect(mocks.command).toHaveBeenCalledWith(
    "save_config",
    expect.objectContaining({ clientId: "claude", expectedRevision: "r1" }),
  );
  expect(localStorage.getItem("vela.config.client")).toBe("claude");
  expect(Object.values(localStorage).join()).not.toContain("fixture-secret");
});
it("keeps invalid source, and protects drafts from other-client and external events", async () => {
  const u = userEvent.setup();
  mount();
  await screen.findByLabelText("默认模型");
  await u.click(screen.getByRole("button", { name: "JSON" }));
  const source = await screen.findByLabelText("JSON 编辑器");
  fireEvent.change(source, { target: { value: '{"env":' } });
  await u.click(screen.getByRole("button", { name: "可视化" }));
  expect(source).toHaveValue('{"env":');
  expect(screen.getByRole("alert")).toBeInTheDocument();
  await act(async () =>
    mocks.events.get("config-state")?.({
      clientId: "codex",
      revision: "new",
      guarded: true,
    }),
  );
  expect(source).toHaveValue('{"env":');
  await act(async () =>
    mocks.events.get("config-state")?.({
      clientId: "claude",
      revision: "new",
      guarded: true,
    }),
  );
  expect(screen.getByText(/磁盘配置已变化/)).toBeInTheDocument();
  expect(source).toHaveValue('{"env":');
});
it("locks managed connection fields and keeps preferences editable", async () => {
  doc.guarded = true;
  const u = userEvent.setup();
  mount();
  await screen.findByLabelText("默认模型");
  await u.click(screen.getByRole("button", { name: "环境变量" }));
  expect(screen.getByLabelText("ANTHROPIC_AUTH_TOKEN")).toBeDisabled();
  expect(screen.getByLabelText("ANTHROPIC_AUTH_TOKEN")).toHaveAttribute(
    "type",
    "password",
  );
  await u.click(screen.getByRole("button", { name: "偏好" }));
  // An absent boolean reads "默认"; the switch then writes an explicit value.
  const memory = screen.getByRole("switch", { name: "自动记忆" });
  expect(memory).not.toBeChecked();
  expect(memory.parentElement).toHaveTextContent("默认");
  await u.click(memory);
  expect(memory).toBeChecked();
  expect(memory.parentElement).toHaveTextContent("开启");
  await u.click(memory);
  expect(memory.parentElement).toHaveTextContent("关闭");
  await u.click(screen.getByRole("button", { name: "保存" }));
  await waitFor(() =>
    expect(JSON.parse(doc.text).autoMemoryEnabled).toBe(false),
  );
  expect(JSON.parse(doc.text).env.ANTHROPIC_AUTH_TOKEN).toBe("fixture-secret");
});
it("restores previous configuration only into the draft and confirms client changes", async () => {
  const u = userEvent.setup();
  mount();
  await screen.findByLabelText("默认模型");
  await u.click(screen.getByLabelText("恢复上次配置"));
  await waitFor(() => expect(screen.getByText("未保存")).toBeInTheDocument());
  expect(
    mocks.command.mock.calls.some(([name]) => name === "save_config"),
  ).toBe(false);
  await u.click(screen.getByRole("button", { name: "Codex" }));
  const dialog = await screen.findByRole("dialog", { name: "确认操作" });
  await u.click(within(dialog).getByRole("button", { name: "取消" }));
  expect(screen.getByText("settings.json")).toBeInTheDocument();
  expect(screen.getByText("未保存")).toBeInTheDocument();
  await u.click(screen.getByLabelText("撤销草稿并重新读取"));
  await u.click(
    within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
      "button",
      { name: "取消" },
    ),
  );
  expect(screen.getByText("未保存")).toBeInTheDocument();
  await u.click(screen.getByLabelText("撤销草稿并重新读取"));
  await u.click(
    within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
      "button",
      { name: "放弃修改" },
    ),
  );
  await waitFor(() =>
    expect(screen.queryByText("未保存")).not.toBeInTheDocument(),
  );
  expect(
    mocks.command.mock.calls.some(([name]) => name === "save_config"),
  ).toBe(false);
});

it("edits Antigravity app config across visual and JSON modes, diffs, restores backup, and guards conflicts", async () => {
  const u = userEvent.setup();
  localStorage.setItem("vela.config.client", "antigravity");
  const enumPrefixes: Record<string, string> = {
    themeMode: "THEME_MODE_",
    autoExecutionPolicy: "CASCADE_COMMANDS_AUTO_EXECUTION_",
  };
  const validPrefixEnum = (val: unknown, prefix: string) =>
    typeof val === "string" &&
    val.startsWith(prefix) &&
    /^[A-Z0-9_]+$/.test(val.slice(prefix.length));
  const appDoc: ConfigDocument = {
    ...doc,
    clientId: "antigravity",
    path: "/fixture/config.json",
    canRestore: true,
    text: '{"userSettings":{"themeMode":"THEME_MODE_DARK","conversationWidth":"CONVERSATION_WIDTH_UNSPECIFIED","autoExecutionPolicy":"eager"},"plugins":{"example.invalid":{"enabled":true}},"future":7}',
  };
  mocks.command.mockImplementation(async (name, args) => {
    if (name === "read_antigravity_config") return { ...appDoc };
    if (name === "validate_antigravity_config") {
      const parsed = get(args.text, []);
      if (
        !parsed ||
        typeof parsed !== "object" ||
        Array.isArray(parsed) ||
        (parsed.userSettings !== undefined &&
          (typeof parsed.userSettings !== "object" ||
            parsed.userSettings === null ||
            Array.isArray(parsed.userSettings)))
      ) {
        throw { code: "CONFIG_TYPE", message: "userSettings 必须是对象" };
      }
      const settings = (parsed.userSettings ?? {}) as Record<string, unknown>;
      for (const [key, prefix] of Object.entries(enumPrefixes)) {
        if (
          settings[key] !== undefined &&
          !validPrefixEnum(settings[key], prefix)
        ) {
          throw {
            code: "CONFIG_TYPE",
            message: `userSettings.${key} 枚举值无效`,
          };
        }
      }
      return;
    }
    if (name === "read_previous_antigravity_config")
      return '{"userSettings":{"themeMode":"THEME_MODE_INHERIT"},"future":7}';
    if (name === "save_antigravity_config")
      throw { code: "CONFLICT", message: "配置已变化，草稿已保留" };
  });
  mount();
  const theme = await screen.findByLabelText("主题模式");
  expect(screen.getByText("config.json")).toBeInTheDocument();
  expect(theme).toHaveValue("THEME_MODE_DARK");
  expect(screen.getByLabelText("对话宽度")).toHaveValue(
    "CONVERSATION_WIDTH_UNSPECIFIED",
  );
  expect(screen.getByRole("option", { name: "未指定（默认）" })).toBeInTheDocument();
  await u.selectOptions(theme, "THEME_MODE_LIGHT");

  // Historical/unknown enum values on disk are visibly flagged and preserved until reselected.
  await u.click(screen.getByRole("button", { name: "执行与安全" }));
  const execPolicy = screen.getByLabelText("命令自动执行策略");
  expect(execPolicy).toHaveValue("eager");
  expect(
    screen.getByRole("option", { name: "未知当前值（eager）" }),
  ).toBeInTheDocument();
  await u.selectOptions(
    execPolicy,
    "CASCADE_COMMANDS_AUTO_EXECUTION_PROCEED_IN_SANDBOX",
  );
  await u.click(screen.getByRole("switch", { name: "终端沙盒" }));

  // Plugin summary renders in the remote/plugins tab, which offers no ADC toggle.
  await u.click(screen.getByRole("button", { name: "远程与插件" }));
  expect(screen.getByText("example.invalid")).toBeInTheDocument();
  expect(screen.getByLabelText("远程控制")).toBeInTheDocument();
  expect(screen.queryByLabelText(/ADC/)).not.toBeInTheDocument();

  // Diff modal works for Antigravity drafts.
  await u.click(screen.getByLabelText("查看改动"));
  const diffDialog = await screen.findByRole("dialog", { name: "配置改动" });
  expect(
    within(diffDialog).getByText("userSettings.themeMode"),
  ).toBeInTheDocument();
  await u.click(within(diffDialog).getByLabelText("关闭"));

  // Switch to JSON mode: official enum values, unknown fields, and plugins remain intact.
  await u.click(screen.getByRole("button", { name: "JSON" }));
  const source = (await screen.findByLabelText(
    "JSON 编辑器",
  )) as HTMLTextAreaElement;
  expect(JSON.parse(source.value)).toEqual({
    userSettings: {
      themeMode: "THEME_MODE_LIGHT",
      conversationWidth: "CONVERSATION_WIDTH_UNSPECIFIED",
      autoExecutionPolicy:
        "CASCADE_COMMANDS_AUTO_EXECUTION_PROCEED_IN_SANDBOX",
      enableTerminalSandbox: true,
    },
    plugins: { "example.invalid": { enabled: true } },
    future: 7,
  });

  // Bogus short enum values (like "dark") and invalid userSettings types are rejected when returning to visual mode.
  fireEvent.change(source, {
    target: { value: '{"userSettings":{"themeMode":"dark"}}' },
  });
  await u.click(screen.getByRole("button", { name: "可视化" }));
  expect(
    await screen.findByText("userSettings.themeMode 枚举值无效"),
  ).toBeInTheDocument();

  fireEvent.change(source, { target: { value: '{"userSettings":0}' } });
  await u.click(screen.getByRole("button", { name: "可视化" }));
  expect(await screen.findByText("userSettings 必须是对象")).toBeInTheDocument();

  // Restore previous backup into draft via Antigravity command.
  await u.click(screen.getByLabelText("恢复上次配置"));
  await u.click(
    within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
      "button",
      { name: "恢复" },
    ),
  );
  await waitFor(() =>
    expect(source).toHaveValue(
      '{"userSettings":{"themeMode":"THEME_MODE_INHERIT"},"future":7}',
    ),
  );

  // External config-state event for antigravity marks conflict without losing draft.
  await act(async () =>
    mocks.events.get("config-state")?.({
      clientId: "antigravity",
      revision: "external-r2",
      guarded: false,
    }),
  );
  expect(screen.getByText(/磁盘配置已变化/)).toBeInTheDocument();

  // Save uses save_antigravity_config and preserves draft on CONFLICT.
  await u.click(screen.getByRole("button", { name: "保存" }));
  expect(mocks.command).toHaveBeenCalledWith("save_antigravity_config", {
    text: '{"userSettings":{"themeMode":"THEME_MODE_INHERIT"},"future":7}',
    expectedRevision: "r1",
  });
  expect(source).toHaveValue(
    '{"userSettings":{"themeMode":"THEME_MODE_INHERIT"},"future":7}',
  );
  expect(await screen.findByText("配置已变化，草稿已保留")).toBeInTheDocument();
  expect(
    mocks.command.mock.calls.some(([name]) => name === "read_config"),
  ).toBe(false);
});

const switcher = () =>
  within(screen.getByRole("navigation", { name: "客户端" }))
    .getAllByRole("button")
    .map((b) => b.textContent);
const order = ["Codex", "Claude Code", "Antigravity", "Grok Build", "DSH"];

it("DSH 视图的切换列包含 Antigravity，顺序固定，可切过去再切回来", async () => {
  const u = userEvent.setup();
  localStorage.setItem("vela.config.client", "dsh");
  mocks.command.mockImplementation(async (name) => {
    if (name === "read_dsh_file")
      return {
        profiles: ["desktop"],
        profile: "desktop",
        key: "profilePatch",
        files: ["profilePatch"],
        text: "[]\n",
        revision: "d1",
        readOnly: false,
        canRestore: false,
      };
    if (name === "read_antigravity_config")
      return {
        ...doc,
        clientId: "antigravity",
        path: "/fixture/config.json",
        text: "{}\n",
      };
  });
  mount();
  await screen.findByLabelText("DSH 配置 profile");
  expect(switcher()).toEqual(order);
  expect(screen.getByRole("button", { name: "DSH" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await u.click(screen.getByRole("button", { name: "Antigravity" }));
  expect(await screen.findByLabelText("Antigravity 配置分组")).toBeVisible();
  expect(switcher()).toEqual(order);
  expect(localStorage.getItem("vela.config.client")).toBe("antigravity");
  await u.click(screen.getByRole("button", { name: "DSH" }));
  expect(await screen.findByLabelText("DSH 配置 profile")).toBeVisible();
  expect(screen.queryByLabelText("Antigravity 配置分组")).toBeNull();
});

const names = () => mocks.command.mock.calls.map(([name]) => name);

it.each([["codex"], ["grok"]])(
  "%s 的可视化与原文共用草稿，切换模式不保存",
  async (clientId) => {
    const u = userEvent.setup();
    localStorage.setItem("vela.config.client", clientId);
    const toml: ConfigDocument = {
      ...doc,
      clientId: clientId as ConfigDocument["clientId"],
      path: "/fixture/config.toml",
      text: "enabled = false\n",
    };
    mocks.command.mockImplementation(async (name, args) => {
      if (name === "read_config") return { ...toml };
      if (name === "read_config_tree")
        return {
          kind: "table",
          entries: [
            ["enabled", { kind: "boolean", value: args.text.includes("true") }],
          ],
        };
      if (name === "patch_config_tree")
        return `${args.path[0]} = ${args.value}\n`;
      if (name === "save_config") return { ...toml, text: args.text };
    });
    mount();
    // Curated groups come first; the file's own keys are under "全部设置".
    await u.click(await screen.findByRole("button", { name: "全部设置" }));
    const toggle = await screen.findByRole("switch", { name: "enabled" });
    expect(screen.getByRole("button", { name: "可视化" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await u.click(toggle);
    await waitFor(() => expect(screen.getByText("未保存")).toBeInTheDocument());
    await u.click(screen.getByRole("button", { name: "原文" }));
    const source = await screen.findByLabelText("TOML 编辑器");
    expect(source).toHaveValue("enabled = true\n");
    fireEvent.change(source, {
      target: { value: "enabled = false\n# note\n" },
    });
    await u.click(screen.getByRole("button", { name: "可视化" }));
    await u.click(await screen.findByRole("button", { name: "全部设置" }));
    expect(
      await screen.findByRole("switch", { name: "enabled" }),
    ).not.toBeChecked();
    expect(names()).not.toContain("save_config");
    await u.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(mocks.command).toHaveBeenCalledWith("save_config", {
        clientId,
        text: "enabled = false\n# note\n",
        expectedRevision: "r1",
      }),
    );
  },
);

it("结构树读不出来时 Codex 留在原文模式并显示原因", async () => {
  localStorage.setItem("vela.config.client", "codex");
  mocks.command.mockImplementation(async (name) => {
    if (name === "read_config")
      return { ...doc, clientId: "codex", text: "model = 1\n" };
    if (name === "read_config_tree")
      throw { code: "CONFIG", message: "浏览器预览不解析 TOML／YAML" };
  });
  const u = userEvent.setup();
  mount();
  expect(await screen.findByLabelText("TOML 编辑器")).toHaveValue(
    "model = 1\n",
  );
  expect(screen.getByRole("alert")).toHaveTextContent(
    "浏览器预览不解析 TOML／YAML",
  );
  await u.click(screen.getByRole("button", { name: "可视化" }));
  expect(screen.getByLabelText("TOML 编辑器")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "原文" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
});

function antigravityFiles(missing = false) {
  localStorage.setItem("vela.config.client", "antigravity");
  const app = {
    ...doc,
    clientId: "antigravity" as const,
    path: "/fixture/config.json",
    text: '{"userSettings":{}}',
  };
  const cli = {
    ...app,
    path: "/fixture/antigravity-cli/settings.json",
    text: missing ? "" : '{"notifications":true}',
    revision: "c1",
    canRestore: !missing,
    missing,
  };
  mocks.command.mockImplementation(async (name, args) => {
    if (name === "read_antigravity_config") return { ...app };
    if (name === "read_antigravity_cli_config") return { ...cli };
    if (name === "read_previous_antigravity_cli_config")
      return '{"notifications":false}';
    if (name === "save_antigravity_cli_config")
      return { ...cli, text: args.text, revision: "c2" };
  });
}
const file = () => screen.getByLabelText("Antigravity 配置文件");

it("Antigravity 按所选文件调用对应命令，并保存 CLI 配置", async () => {
  const u = userEvent.setup();
  antigravityFiles();
  mount();
  await screen.findByLabelText("Antigravity 配置分组");
  expect(names()).not.toContain("read_antigravity_cli_config");
  await u.selectOptions(file(), "CLI 配置");
  const form = await screen.findByLabelText("CLI 表单");
  expect(form).toHaveValue('{"notifications":true}');
  expect(screen.getByText("settings.json")).toBeInTheDocument();
  fireEvent.change(form, { target: { value: '{"notifications":false}' } });
  await u.click(screen.getByRole("button", { name: "JSON" }));
  expect(await screen.findByLabelText("JSON 编辑器")).toHaveValue(
    '{"notifications":false}',
  );
  await u.click(screen.getByRole("button", { name: "可视化" }));
  await screen.findByLabelText("CLI 表单");
  expect(mocks.command).toHaveBeenCalledWith(
    "validate_antigravity_cli_config",
    { text: '{"notifications":false}' },
  );
  await u.click(screen.getByRole("button", { name: "保存" }));
  await waitFor(() =>
    expect(mocks.command).toHaveBeenCalledWith("save_antigravity_cli_config", {
      text: '{"notifications":false}',
      expectedRevision: "c1",
    }),
  );
  await u.click(screen.getByLabelText("恢复上次配置"));
  await waitFor(() =>
    expect(names()).toContain("read_previous_antigravity_cli_config"),
  );
  expect(names()).not.toContain("save_antigravity_config");
  expect(names()).not.toContain("read_previous_antigravity_config");
});

it("CLI 配置文件不存在时显示空状态，不能保存", async () => {
  const u = userEvent.setup();
  antigravityFiles(true);
  mount();
  await screen.findByLabelText("Antigravity 配置分组");
  await u.selectOptions(file(), "CLI 配置");
  expect(
    await screen.findByText(
      "尚未找到 Antigravity CLI 配置文件，首次运行 agy 后生成",
    ),
  ).toBeInTheDocument();
  expect(screen.queryByLabelText("CLI 表单")).toBeNull();
  expect(screen.queryByRole("button", { name: "JSON" })).toBeNull();
  expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
  expect(screen.getByLabelText("恢复上次配置")).toBeDisabled();
  fireEvent.keyDown(window, { key: "s", ctrlKey: true });
  expect(names()).not.toContain("save_antigravity_cli_config");
});

it("切换 Antigravity 文件前确认未保存的修改", async () => {
  const u = userEvent.setup();
  antigravityFiles();
  mount();
  await screen.findByLabelText("Antigravity 配置分组");
  await u.click(screen.getByRole("button", { name: "远程与插件" }));
  await u.type(screen.getByLabelText("远程控制主机名"), "h");
  expect(screen.getByText("未保存")).toBeInTheDocument();
  await u.selectOptions(file(), "CLI 配置");
  await u.click(
    within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
      "button",
      { name: "取消" },
    ),
  );
  expect(file()).toHaveValue("app");
  expect(screen.getByText("未保存")).toBeInTheDocument();
  expect(names()).not.toContain("read_antigravity_cli_config");
  await u.selectOptions(file(), "CLI 配置");
  await u.click(
    within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
      "button",
      { name: "放弃修改" },
    ),
  );
  expect(await screen.findByLabelText("CLI 表单")).toBeInTheDocument();
  expect(file()).toHaveValue("cli");
});
