import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import type { DshFileView } from "./bridge-contracts";
const mock = vi.hoisted(() => ({ command: vi.fn(), confirm: vi.fn() }));
vi.mock("./bridge", () => ({ command: mock.command }));
vi.mock("./confirmation", () => ({ confirmAction: mock.confirm }));
vi.mock("@uiw/react-codemirror", () => ({
  default: ({
    value,
    onChange,
    editable,
    "aria-label": label,
  }: {
    value: string;
    onChange: (value: string) => void;
    editable: boolean;
    "aria-label": string;
  }) => (
    <textarea
      value={value}
      onChange={(e) => onChange(e.target.value)}
      readOnly={!editable}
      aria-label={label}
    />
  ),
}));
import DshConfigEditor from "./DshConfigEditor";
let doc: DshFileView;
beforeEach(() => {
  localStorage.clear();
  doc = {
    profiles: ["desktop", "default"],
    profile: "desktop",
    key: "profilePatch",
    files: ["homePatch", "profilePatch", "environment", "credentials"],
    text: "[]\n",
    revision: "r1",
    readOnly: false,
    canRestore: true,
  };
  mock.confirm.mockReset().mockResolvedValue(false);
  mock.command.mockReset().mockImplementation(async (name, args) => {
    if (name === "read_dsh_file")
      return {
        ...doc,
        profile: args.profile ?? "desktop",
        key: args.key,
        readOnly: args.key === "credentials",
        text: args.key === "credentials" ? "refs.FIXTURE: ••••" : doc.text,
      };
    if (name === "save_dsh_file") throw { message: "文件已被其他程序修改" };
    if (name === "read_previous_dsh_file") return "# prior\n[]\n";
    if (name === "restore_dsh_file") {
      doc = { ...doc, text: "# prior\n[]\n", revision: "r2" };
      return doc;
    }
    // Without a structure tree the editor stays on the source, as in the preview.
    if (name === "read_config_tree")
      throw { code: "CONFIG", message: "此处不解析 YAML" };
    throw Error("unexpected fixture command");
  });
});
const lastRead = () =>
  mock.command.mock.calls
    .filter(([name]) => name === "read_dsh_file")
    .at(-1)?.[1];
function mount() {
  render(
    <DshConfigEditor
      theme="dark"
      select={() => {}}
      onDirty={() => {}}
      onMessage={() => {}}
    />,
  );
}
it("keeps the draft after conflict and confirms before profile/file switches", async () => {
  mount();
  const user = userEvent.setup();
  await waitFor(() =>
    expect(screen.getByRole("textbox", { name: "DSH 配置内容" })).toHaveValue(
      "[]\n",
    ),
  );
  fireEvent.change(screen.getByRole("textbox", { name: "DSH 配置内容" }), {
    target: { value: "# draft\n[]\n" },
  });
  await user.click(screen.getByRole("button", { name: "保存" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("草稿已保留");
  await user.selectOptions(
    screen.getByLabelText("DSH 配置 profile"),
    "default",
  );
  expect(mock.confirm).toHaveBeenCalledOnce();
  expect(screen.getByRole("textbox", { name: "DSH 配置内容" })).toHaveValue(
    "# draft\n[]\n",
  );
  expect(screen.getByLabelText("DSH 配置 profile")).toHaveValue("desktop");
});
it("restores through the revision-checked command and makes credentials readonly", async () => {
  mount();
  const user = userEvent.setup();
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "恢复上一版" })).toBeEnabled(),
  );
  mock.confirm.mockResolvedValue(true);
  await user.click(screen.getByRole("button", { name: "恢复上一版" }));
  expect(mock.command).toHaveBeenCalledWith("restore_dsh_file", {
    profile: "desktop",
    key: "profilePatch",
    expectedRevision: "r1",
  });
  await user.selectOptions(
    screen.getByLabelText("DSH 配置文件"),
    "credentials",
  );
  await waitFor(() =>
    expect(
      screen.getByRole("textbox", { name: "DSH 配置内容" }),
    ).toHaveAttribute("readonly"),
  );
  expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
});

it("keeps file and profile choices available after a read error", async () => {
  mount();
  const user = userEvent.setup();
  await waitFor(() =>
    expect(screen.getByLabelText("DSH 配置文件")).toBeEnabled(),
  );
  mock.command.mockRejectedValueOnce({ message: "文件不可读取" });
  await user.selectOptions(
    screen.getByLabelText("DSH 配置文件"),
    "credentials",
  );
  expect(await screen.findByRole("alert")).toHaveTextContent("文件不可读取");
  await user.selectOptions(
    screen.getByLabelText("DSH 配置 profile"),
    "desktop",
  );
  await waitFor(() =>
    expect(screen.getByLabelText("DSH 配置文件")).toBeEnabled(),
  );
  await user.selectOptions(
    screen.getByLabelText("DSH 配置文件"),
    "profilePatch",
  );
  await waitFor(() =>
    expect(screen.getByRole("textbox", { name: "DSH 配置内容" })).toHaveValue(
      "[]\n",
    ),
  );
  await user.selectOptions(screen.getByLabelText("DSH 配置 profile"), "");
  await waitFor(() =>
    expect(screen.getByLabelText("DSH 配置文件")).toHaveValue("homePatch"),
  );
});

it("opens the desktop patch and switches the file when choosing a different profile", async () => {
  mount();
  const user = userEvent.setup();
  await waitFor(() =>
    expect(screen.getByLabelText("DSH 配置 profile")).toHaveValue("desktop"),
  );
  expect(screen.getByLabelText("DSH 配置文件")).toHaveValue("profilePatch");
  await user.selectOptions(screen.getByLabelText("DSH 配置文件"), "homePatch");
  await user.selectOptions(
    screen.getByLabelText("DSH 配置 profile"),
    "default",
  );
  await waitFor(() =>
    expect(lastRead()).toEqual({
      profile: "default",
      key: "profilePatch",
    }),
  );
});

it("remembers only an actively selected profile across editor mounts", async () => {
  const user = userEvent.setup();
  const props = {
    theme: "dark" as const,
    select: () => {},
    onDirty: () => {},
    onMessage: () => {},
  };
  const first = render(<DshConfigEditor {...props} />);
  await waitFor(() =>
    expect(screen.getByLabelText("DSH 配置 profile")).toHaveValue("desktop"),
  );
  await user.selectOptions(
    screen.getByLabelText("DSH 配置 profile"),
    "default",
  );
  await waitFor(() =>
    expect(screen.getByLabelText("DSH 配置 profile")).toHaveValue("default"),
  );
  first.unmount();
  render(<DshConfigEditor {...props} />);
  await waitFor(() =>
    expect(screen.getByLabelText("DSH 配置 profile")).toHaveValue("default"),
  );
  expect(lastRead()).toEqual({
    profile: "default",
    key: "profilePatch",
  });
});

function withTree() {
  const read = mock.command.getMockImplementation()!;
  mock.command.mockImplementation(async (name, args) => {
    if (name === "read_config_tree")
      return {
        kind: "table",
        entries: [
          ["enabled", { kind: "boolean", value: args.text.includes("true") }],
        ],
      };
    if (name === "patch_config_tree") return `${args.path[0]}: ${args.value}\n`;
    return read(name, args);
  });
}

it("YAML 文件有可视化模式，并与原文共用草稿", async () => {
  doc.text = "enabled: false\n";
  withTree();
  mount();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("switch", { name: "enabled" }));
  expect(mock.command).toHaveBeenCalledWith("patch_config_tree", {
    format: "yaml",
    text: "enabled: false\n",
    path: ["enabled"],
    value: true,
  });
  await user.click(screen.getByRole("button", { name: "原文" }));
  const source = await screen.findByRole("textbox", { name: "DSH 配置内容" });
  expect(source).toHaveValue("enabled: true\n");
  expect(screen.getByText(/未保存/)).toBeInTheDocument();
  fireEvent.change(source, { target: { value: "enabled: false\n# note\n" } });
  await user.click(screen.getByRole("button", { name: "可视化" }));
  expect(
    await screen.findByRole("switch", { name: "enabled" }),
  ).not.toBeChecked();
  expect(
    mock.command.mock.calls.some(([name]) => name === "save_dsh_file"),
  ).toBe(false);
});

it(".env 和凭据文件没有可视化", async () => {
  withTree();
  mount();
  const user = userEvent.setup();
  await screen.findByRole("switch", { name: "enabled" });
  for (const key of ["environment", "credentials"]) {
    await user.selectOptions(screen.getByLabelText("DSH 配置文件"), key);
    expect(
      await screen.findByRole("textbox", { name: "DSH 配置内容" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "可视化" })).toBeNull();
    expect(screen.queryByRole("switch")).toBeNull();
  }
});
