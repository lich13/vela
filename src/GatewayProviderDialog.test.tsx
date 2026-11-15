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
