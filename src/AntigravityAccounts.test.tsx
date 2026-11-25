import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import AntigravityAccounts from "./AntigravityAccounts";
import { subscribe } from "./bridge";
import type { AntigravityAuthState } from "./bridge-contracts";

const mockCommand = vi.fn();
const listeners = new Map<string, (payload: unknown) => void>();

vi.mock("./bridge", () => ({
  command: (...args: unknown[]) => mockCommand(...args),
  subscribe: vi.fn(async (event: string, cb: (payload: unknown) => void) => {
    listeners.set(event, cb);
    return () => {
      listeners.delete(event);
    };
  }),
}));

const initialState: AntigravityAuthState = {
  revision: "rev-1",
  oauthPresent: true,
  oauthEmail: "user1@example.invalid",
  oauthAuthMethod: "google-oauth",
  appRestarted: false,
  accounts: [
    {
      id: "oauth-1",
      name: "OAuth 主号",
      email: "user1@example.invalid",
      sourceType: "google-oauth",
      updatedAt: 1710000000,
      current: true,
      credentialRevision: "cred-1",
    },
    {
      id: "oauth-2",
      name: "OAuth 备号",
      email: "user2@example.invalid",
      sourceType: "jetski-sso",
      updatedAt: 1710000010,
      current: false,
      credentialRevision: "cred-2",
    },
  ],
};

const row = (name: string) =>
  screen.getByText(name).closest<HTMLElement>(".account-row")!;

describe("AntigravityAccounts", () => {
  afterEach(() => {
    cleanup();
    mockCommand.mockReset();
    listeners.clear();
  });

  it("keeps account state on credential read failure and retries manually", async () => {
    const setError = vi.fn();
    mockCommand.mockResolvedValue(initialState);
    render(<AntigravityAccounts notify={() => {}} setError={setError} />);
    await screen.findByText("OAuth 主号");
    mockCommand.mockRejectedValueOnce({
      code: "AUTH",
      message: "无法读取 macOS 钥匙串中的 Antigravity 登录凭据",
    });
    fireEvent.click(
      screen.getByRole("button", { name: "刷新 Antigravity 认证状态" }),
    );
    await waitFor(() =>
      expect(setError).toHaveBeenCalledWith(
        "无法读取 macOS 钥匙串中的 Antigravity 登录凭据",
      ),
    );
    expect(screen.getByText("OAuth 主号")).toBeInTheDocument();
    expect(screen.getByText("OAuth 备号")).toBeInTheDocument();
    mockCommand.mockResolvedValue({
      ...initialState,
      accounts: initialState.accounts.map((a) => ({
        ...a,
        name: a.id === "oauth-1" ? "刷新后的主号" : a.name,
      })),
    });
    fireEvent.click(
      screen.getByRole("button", { name: "刷新 Antigravity 认证状态" }),
    );
    await screen.findByText("刷新后的主号");
  });

  it("lists only OAuth accounts, each with switch, rename and delete", async () => {
    mockCommand.mockResolvedValue(initialState);
    render(<AntigravityAccounts notify={vi.fn()} setError={vi.fn()} />);
    await screen.findByText("OAuth 主号");

    const list = screen.getByLabelText("Antigravity 账号列表");
    expect(list.querySelectorAll(".account-row")).toHaveLength(2);
    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(
      "已保存账号 2",
    );

    const current = row("OAuth 主号");
    expect(current).toHaveClass("selected");
    expect(
      within(current).getByText("user1@example.invalid"),
    ).toBeInTheDocument();
    expect(within(current).getByText("当前生效")).toBeInTheDocument();
    expect(
      within(current).queryByRole("button", { name: "切换到 OAuth 主号" }),
    ).not.toBeInTheDocument();

    const other = row("OAuth 备号");
    expect(other).not.toHaveClass("selected");
    expect(
      within(other).getByRole("button", { name: "切换到 OAuth 备号" }),
    ).toBeEnabled();
    for (const item of [current, other])
      expect(
        [...item.querySelectorAll(".menu-popover button")].map(
          (button) => button.textContent,
        ),
      ).toEqual(["重命名", "删除"]);

    // Import, credential editing, mode switching and type labels are gone.
    expect(
      screen.queryByRole("button", { name: "导入账号" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "编辑凭据" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("group", { name: "切换认证模式" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("tablist")).not.toBeInTheDocument();
    expect(screen.queryAllByText(/模式|ADC|Vertex|OAuth ·/)).toHaveLength(0);
    expect(
      mockCommand.mock.calls.every(
        ([name]) => name === "get_antigravity_auth_state",
      ),
    ).toBe(true);
  });

  it("opens with the heading it is given, then import and export as a pair and a refresh button", async () => {
    mockCommand.mockResolvedValue(initialState);
    const view = render(
      <AntigravityAccounts
        heading={<h1>账号</h1>}
        notify={vi.fn()}
        setError={vi.fn()}
      />,
    );
    await screen.findByText("OAuth 主号");
    const row = view.container.querySelector<HTMLElement>(".page-heading")!;
    expect(within(row).getByRole("heading", { level: 1 })).toHaveTextContent(
      "账号",
    );
    const pair = within(row).getByRole("group", { name: "账号导入导出" });
    expect(pair).toHaveClass("exchange-button-group");
    expect(
      within(pair)
        .getAllByRole("button")
        .map((button) => [button.textContent, button.className]),
    ).toEqual([
      ["导入", "exchange-button"],
      ["导出", "exchange-button"],
    ]);
    const refresh = within(row).getByRole("button", {
      name: "刷新 Antigravity 认证状态",
    });
    expect(refresh).toHaveTextContent("刷新");
    expect(pair).not.toContainElement(refresh);
  });

  it("搜索按名称和邮箱过滤，不区分大小写；没有匹配时沿用 Codex 列表的说法", async () => {
    mockCommand.mockResolvedValue(initialState);
    render(<AntigravityAccounts notify={vi.fn()} setError={vi.fn()} />);
    await screen.findByText("OAuth 主号");
    const search = screen.getByRole("textbox", { name: "搜索账号" });
    expect(search).toHaveAttribute("placeholder", "搜索账号");
    expect(search.closest("label")).toHaveClass("search");
    const names = () =>
      [...document.querySelectorAll(".account-row .account-title strong")].map(
        (node) => node.textContent,
      );

    fireEvent.change(search, { target: { value: "USER2@EXAMPLE" } });
    expect(names()).toEqual(["OAuth 备号"]);
    fireEvent.change(search, { target: { value: "oauth 主" } });
    expect(names()).toEqual(["OAuth 主号"]);
    // The count is of saved accounts, not of the rows left by the search.
    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(
      "已保存账号 2",
    );

    fireEvent.change(search, { target: { value: "不存在" } });
    expect(names()).toEqual([]);
    expect(
      screen.getByRole("heading", { name: "没有找到匹配账号" }),
    ).toBeInTheDocument();
    expect(screen.getByText("尝试其他名称或邮箱")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "导出 Antigravity 账号" }),
    ).toBeEnabled();

    fireEvent.change(search, { target: { value: "" } });
    expect(names()).toEqual(["OAuth 主号", "OAuth 备号"]);
  });

  it("说明切换会关闭运行中的 agy 会话，并报告关闭了几个", async () => {
    const notify = vi.fn();
    mockCommand.mockImplementation(async (name: string) =>
      name === "switch_antigravity_account"
        ? { ...initialState, revision: "rev-2", cliClosed: 2 }
        : { ...initialState, cliSessions: 2 },
    );
    render(<AntigravityAccounts notify={notify} setError={vi.fn()} />);
    expect(await screen.findByRole("note")).toHaveTextContent(
      /有 2 个 agy 会话正在运行.*切换账号时\s*Vela 会一并关闭这些会话/,
    );
    fireEvent.click(screen.getByRole("button", { name: "切换到 OAuth 备号" }));
    await waitFor(() =>
      expect(notify).toHaveBeenCalledWith(
        "已切换到 OAuth 备号；已关闭 2 个 agy 会话",
      ),
    );
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
  });

  it("导入导出账号文件，并报告结果；取消对话框时不提示", async () => {
    const notify = vi.fn();
    const imported = {
      ...initialState,
      revision: "rev-2",
      accounts: [
        ...initialState.accounts,
        {
          ...initialState.accounts[0],
          id: "imported",
          name: "导入的账号",
          current: false,
        },
      ],
    };
    const results: Record<string, unknown[]> = {
      export_antigravity_accounts: [null, 1, 2],
      import_antigravity_accounts: [
        { state: imported, added: 1, skipped: 2 },
        { state: imported, added: 0, skipped: 3 },
      ],
    };
    mockCommand.mockImplementation(async (name: string) =>
      name in results ? results[name].shift() : initialState,
    );
    render(<AntigravityAccounts notify={notify} setError={vi.fn()} />);
    const exporting = await screen.findByRole("button", {
      name: "导出 Antigravity 账号",
    });
    await waitFor(() => expect(exporting).toBeEnabled());
    fireEvent.click(exporting);
    let picker = screen.getByRole("dialog", { name: "导出账号" });
    const all = () => within(picker).getByRole("checkbox", { name: /全选/ });
    const submit = () =>
      within(picker).getByRole("button", { name: "导出所选" });
    expect(all()).toBeChecked();
    expect(within(picker).getByText("已选 2")).toBeInTheDocument();
    expect(
      within(picker).getByText("user2@example.invalid"),
    ).toBeInTheDocument();
    fireEvent.click(
      within(picker).getByRole("checkbox", { name: /OAuth 备号/ }),
    );
    expect(all()).not.toBeChecked();
    expect(within(picker).getByText("已选 1")).toBeInTheDocument();

    // Dismissing the save dialog keeps the picker and its selection.
    fireEvent.click(submit());
    await waitFor(() => expect(submit()).toBeEnabled());
    expect(notify).not.toHaveBeenCalled();
    expect(mockCommand).toHaveBeenLastCalledWith(
      "export_antigravity_accounts",
      {
        instance: "local",
        ids: ["oauth-1"],
        expectedRevision: initialState.revision,
      },
    );
    fireEvent.click(submit());
    await waitFor(() =>
      expect(notify).toHaveBeenLastCalledWith(
        "已导出 1 个账号；文件包含登录凭据，请妥善保管",
      ),
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    // Reopening starts from everything selected again.
    fireEvent.click(exporting);
    picker = screen.getByRole("dialog", { name: "导出账号" });
    expect(all()).toBeChecked();
    fireEvent.click(all());
    expect(within(picker).getByText("已选 0")).toBeInTheDocument();
    expect(submit()).toBeDisabled();
    fireEvent.click(all());
    fireEvent.click(submit());
    await waitFor(() =>
      expect(notify).toHaveBeenLastCalledWith(
        "已导出 2 个账号；文件包含登录凭据，请妥善保管",
      ),
    );
    expect(mockCommand).toHaveBeenLastCalledWith(
      "export_antigravity_accounts",
      {
        instance: "local",
        ids: ["oauth-1", "oauth-2"],
        expectedRevision: initialState.revision,
      },
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    const importing = screen.getByRole("button", {
      name: "导入 Antigravity 账号",
    });
    fireEvent.click(importing);
    await waitFor(() =>
      expect(notify).toHaveBeenLastCalledWith(
        "已导入 1 个账号，2 个账号已存在，未改动",
      ),
    );
    expect(await screen.findByText("导入的账号")).toBeInTheDocument();
    fireEvent.click(importing);
    await waitFor(() =>
      expect(notify).toHaveBeenLastCalledWith(
        "没有新账号可导入，3 个账号已存在",
      ),
    );
    expect(mockCommand).toHaveBeenLastCalledWith(
      "import_antigravity_accounts",
      { instance: "local", expectedRevision: "rev-2" },
    );
  });

  it("导出失败时错误留在勾选窗口内，并重新读取账号状态", async () => {
    const setError = vi.fn();
    mockCommand.mockImplementation(async (name: string) => {
      if (name === "export_antigravity_accounts")
        throw { code: "CONFLICT", message: "Antigravity 凭据状态已变更" };
      return initialState;
    });
    render(<AntigravityAccounts notify={vi.fn()} setError={setError} />);
    await screen.findByText("OAuth 主号");
    fireEvent.click(
      screen.getByRole("button", { name: "导出 Antigravity 账号" }),
    );
    const picker = screen.getByRole("dialog", { name: "导出账号" });
    fireEvent.click(within(picker).getByRole("button", { name: "导出所选" }));
    expect(await within(picker).findByRole("alert")).toHaveTextContent(
      "Antigravity 凭据状态已变更",
    );
    expect(setError).not.toHaveBeenCalledWith("Antigravity 凭据状态已变更");
    expect(mockCommand).toHaveBeenLastCalledWith("get_antigravity_auth_state");
    fireEvent.click(within(picker).getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("switches, renames and deletes against the latest revision", async () => {
    const notify = vi.fn();
    mockCommand.mockImplementation(async (name: string) => {
      if (name === "switch_antigravity_account")
        return {
          ...initialState,
          revision: "rev-2",
          appRestarted: true,
          accounts: initialState.accounts.map((a) => ({
            ...a,
            current: a.id === "oauth-2",
          })),
        };
      if (name === "rename_antigravity_account")
        return {
          ...initialState,
          revision: "rev-3",
          accounts: initialState.accounts.map((a) => ({
            ...a,
            current: a.id === "oauth-2",
            name: a.id === "oauth-1" ? "改名后的主号" : a.name,
          })),
        };
      if (name === "delete_antigravity_account")
        return {
          ...initialState,
          revision: "rev-4",
          accounts: [{ ...initialState.accounts[1], current: true }],
        };
      return initialState;
    });
    render(<AntigravityAccounts notify={notify} setError={vi.fn()} />);
    await screen.findByText("OAuth 主号");

    fireEvent.click(screen.getByRole("button", { name: "切换到 OAuth 备号" }));
    await waitFor(() =>
      expect(mockCommand).toHaveBeenCalledWith("switch_antigravity_account", {
        id: "oauth-2",
        expectedRevision: "rev-1",
      }),
    );
    await waitFor(() => expect(row("OAuth 备号")).toHaveClass("selected"));
    expect(notify).toHaveBeenLastCalledWith(
      "已切换到 OAuth 备号 并自动重启 Antigravity",
    );

    fireEvent.click(within(row("OAuth 主号")).getByText("重命名"));
    const rename = screen.getByRole("dialog", { name: "重命名账号" });
    expect(within(rename).getByLabelText("新名称")).toHaveValue("OAuth 主号");
    fireEvent.change(within(rename).getByLabelText("新名称"), {
      target: { value: "改名后的主号" },
    });
    fireEvent.click(within(rename).getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(mockCommand).toHaveBeenCalledWith("rename_antigravity_account", {
        id: "oauth-1",
        name: "改名后的主号",
        expectedRevision: "rev-2",
      }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(notify).toHaveBeenLastCalledWith("已重命名账号");

    fireEvent.click(within(row("改名后的主号")).getByText("删除"));
    const remove = screen.getByRole("dialog", { name: "删除账号" });
    fireEvent.click(within(remove).getByRole("button", { name: "删除" }));
    await waitFor(() =>
      expect(mockCommand).toHaveBeenCalledWith("delete_antigravity_account", {
        id: "oauth-1",
        expectedRevision: "rev-3",
      }),
    );
    await waitFor(() =>
      expect(screen.queryByText("改名后的主号")).not.toBeInTheDocument(),
    );
    expect(notify).toHaveBeenLastCalledWith("已删除账号");
  });

  it("keeps a failed rename in the dialog with its error", async () => {
    const setError = vi.fn();
    mockCommand.mockImplementation(async (name: string) => {
      if (name === "rename_antigravity_account")
        throw { code: "CONFLICT", message: "Antigravity 凭据状态已变更" };
      return initialState;
    });
    render(<AntigravityAccounts notify={vi.fn()} setError={setError} />);
    await screen.findByText("OAuth 主号");
    fireEvent.click(within(row("OAuth 备号")).getByText("重命名"));
    fireEvent.change(screen.getByLabelText("新名称"), {
      target: { value: "草稿名称" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Antigravity 凭据状态已变更",
    );
    expect(screen.getByLabelText("新名称")).toHaveValue("草稿名称");
    expect(setError).not.toHaveBeenCalledWith("Antigravity 凭据状态已变更");
  });

  it("explains how an account appears when none is saved", async () => {
    mockCommand.mockResolvedValue({
      ...initialState,
      oauthPresent: false,
      oauthEmail: null,
      oauthAuthMethod: null,
      accounts: [],
    });
    render(<AntigravityAccounts notify={vi.fn()} setError={vi.fn()} />);
    expect(
      await screen.findByRole("heading", {
        name: "尚无已保存的 Antigravity 账号",
      }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "在 Antigravity app 内登录后，Vela 会自动识别并加入切换列表。",
      ),
    ).toBeInTheDocument();
    const list = screen.getByLabelText("Antigravity 账号列表");
    expect(list.querySelector(".account-row")).toBeNull();
    expect(within(list).queryByRole("button")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "导入账号" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "导出 Antigravity 账号" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "刷新 Antigravity 认证状态" }),
    ).toBeEnabled();
  });

  it("auto-updates accounts from antigravity-auth-state events", async () => {
    const notify = vi.fn();
    mockCommand.mockResolvedValue(initialState);
    render(<AntigravityAccounts notify={notify} setError={vi.fn()} />);
    await screen.findByText("OAuth 主号");

    act(() => {
      listeners.get("antigravity-auth-state")?.({
        ...initialState,
        revision: "rev-auto",
        oauthEmail: "user3@example.invalid",
        accounts: [
          ...initialState.accounts.map((a) => ({ ...a, current: false })),
          {
            id: "oauth-3",
            name: "OAuth 新号",
            email: "user3@example.invalid",
            sourceType: "google-oauth",
            updatedAt: 1710000099,
            current: true,
            credentialRevision: "cred-auto",
          },
        ],
      });
    });
    expect(row("OAuth 新号")).toHaveClass("selected");
    expect(row("OAuth 主号")).not.toHaveClass("selected");
    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(
      "已保存账号 3",
    );
    // The new-account notice is delivered by the backend, not by this view.
    expect(notify).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "切换到 OAuth 主号" }));
    await waitFor(() =>
      expect(mockCommand).toHaveBeenCalledWith("switch_antigravity_account", {
        id: "oauth-1",
        expectedRevision: "rev-auto",
      }),
    );
  });

  it("keeps newer events when the initial state read finishes late", async () => {
    let finish!: (state: AntigravityAuthState) => void;
    mockCommand.mockReturnValue(
      new Promise<AntigravityAuthState>((resolve) => {
        finish = resolve;
      }),
    );
    render(<AntigravityAccounts notify={vi.fn()} setError={vi.fn()} />);
    act(() =>
      listeners.get("antigravity-auth-state")?.({
        ...initialState,
        accounts: [{ ...initialState.accounts[0], name: "最新账号" }],
      }),
    );
    await act(async () => {
      finish(initialState);
    });
    expect(screen.getByText("最新账号")).toBeInTheDocument();
    expect(screen.queryByText("OAuth 主号")).not.toBeInTheDocument();
  });

  it("disposes event registration that finishes after unmount", async () => {
    let registered!: (clean: () => void) => void;
    vi.mocked(subscribe).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          registered = resolve;
        }),
    );
    mockCommand.mockResolvedValue(initialState);
    const view = render(
      <AntigravityAccounts notify={vi.fn()} setError={vi.fn()} />,
    );
    await screen.findByText("OAuth 主号");
    view.unmount();
    const clean = vi.fn();
    await act(async () => {
      registered(clean);
    });
    expect(clean).toHaveBeenCalledTimes(1);
  });
});
