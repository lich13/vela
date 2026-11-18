import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import GatewayProviderDialog from "./GatewayProviderDialog";
import { claudeGatewayDemo } from "./gateway-preview";

vi.mock("./bridge", () => ({
  command: vi.fn(),
  subscribe: vi.fn(async () => () => {}),
}));

describe("Claude provider message_stop setting", () => {
  it.each([undefined, false, true])(
    "saves an independent value from %s",
    async (initial) => {
      const user = userEvent.setup();
      const save = vi.fn().mockResolvedValue(undefined);
      const dirty = vi.fn();
      const provider =
        initial === undefined
          ? undefined
          : {
              ...claudeGatewayDemo.providers[0],
              endOnMessageStop: initial,
            };
      render(
        <GatewayProviderDialog
          clientId="claude"
          connectionMode="bearer"
          dialog={{ kind: "provider", item: provider }}
          revision="fixture-revision"
          close={() => {}}
          save={save}
          onDirtyChange={dirty}
        />,
      );
      const control = screen.getByRole("checkbox", {
        name: "收到 message_stop 后立即结束响应",
      });
      expect(control).toHaveProperty("checked", Boolean(initial));
      if (!provider) {
        await user.type(
          screen.getByLabelText("ANTHROPIC_BASE_URL"),
          "https://fixture.example.invalid",
        );
        await user.type(
          screen.getByLabelText("ANTHROPIC_AUTH_TOKEN"),
          "fixture-token",
        );
      }
      await user.click(control);
      expect(dirty).toHaveBeenLastCalledWith(true);
      await user.click(screen.getByRole("button", { name: "保存" }));
      expect(save).toHaveBeenCalledWith(
        expect.objectContaining({
          op: "saveProvider",
          id: provider?.id ?? null,
          endOnMessageStop: !initial,
        }),
        "fixture-revision",
      );
      expect(save.mock.calls[0][0]).not.toHaveProperty("searchOnly");
    },
  );
});

describe("Claude reliability controls", () => {
  it("tracks drafts, persists independent controls and retains values after a failed save", async () => {
    const user = userEvent.setup();
    const dirty = vi.fn();
    const save = vi
      .fn()
      .mockRejectedValueOnce(new Error("fixture save failed"))
      .mockResolvedValueOnce(undefined);
    const item = { ...claudeGatewayDemo.providers[0], upstreamRetrySeconds: 5 };
    render(
      <GatewayProviderDialog
        clientId="claude"
        connectionMode="bearer"
        dialog={{ kind: "provider", item }}
        revision="fixture-revision"
        close={() => {}}
        save={save}
        onDirtyChange={dirty}
      />,
    );
    expect(
      screen.queryByRole("spinbutton", { name: "风控重试 秒" }),
    ).toBeNull();
    for (const name of ["过滤 Token 计数请求", "完整响应后发送", "无尽模式"])
      await user.click(screen.getByRole("checkbox", { name }));
    const interval = screen.getByRole("spinbutton", { name: "风控重试 秒" });
    expect(interval).toHaveValue(5);
    await user.clear(interval);
    await user.type(interval, "7");
    const normal = screen.getByRole("spinbutton", { name: "普通重试 秒" });
    expect(normal).toHaveValue(3);
    await user.clear(normal);
    await user.type(normal, "4");
    expect(dirty).toHaveBeenLastCalledWith(true);
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "fixture save failed",
    );
    expect(interval).toHaveValue(7);
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(save).toHaveBeenLastCalledWith(
      expect.objectContaining({
        filterCountTokens: true,
        bufferCompleteResponse: true,
        endlessMode: true,
        upstreamRetrySeconds: 7,
        normalRetrySeconds: 4,
      }),
      null,
    );
  });
});
