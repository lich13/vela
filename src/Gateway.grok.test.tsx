import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ClientId, GatewayState } from "./types";

const mock = vi.hoisted(() => ({
  command: vi.fn(),
  confirmAction: vi.fn(async () => true),
}));

vi.mock("./bridge", () => ({
  command: (name: string, args: unknown) =>
    name === "get_gateway_freeze"
      ? Promise.resolve({
          clients: ["codex", "claude", "grok", "dsh"].map((clientId) => ({
            clientId,
            frozen: false,
          })),
        })
      : mock.command(name, args),
  subscribe: vi.fn(async () => () => {}),
}));
vi.mock("./confirmation", () => ({ confirmAction: mock.confirmAction }));

import Gateway from "./Gateway";
import { gatewayDemo, grokGatewayDemo } from "./gateway-preview";

let codex: GatewayState;
let grok: GatewayState;

beforeEach(() => {
  localStorage.clear();
  codex = { ...structuredClone(gatewayDemo), providers: [] };
  grok = { ...structuredClone(grokGatewayDemo), providers: [] };
  mock.command.mockReset();
  mock.confirmAction.mockReset().mockResolvedValue(true);
  mock.command.mockImplementation(
    async (name: string, args?: { clientId?: ClientId }) => {
      const state = args?.clientId === "grok" ? grok : codex;
      if (name === "get_gateway" || name === "update_gateway")
        return structuredClone(state);
      throw new Error(`Unexpected fixture command: ${name}`);
    },
  );
});

afterEach(() => localStorage.clear());

it("switches to Grok and saves a provider with the selected upstream protocol", async () => {
  const user = userEvent.setup();
  render(<Gateway notify={() => {}} />);

  await screen.findByRole("button", { name: "添加" });
  expect(
    within(screen.getByRole("navigation", { name: "客户端" })).getByRole(
      "button",
      { name: "Codex" },
    ),
  ).toHaveAttribute("aria-pressed", "true");
  await user.click(
    within(screen.getByRole("navigation", { name: "客户端" })).getByRole(
      "button",
      { name: "Grok Build" },
    ),
  );
  await waitFor(() =>
    expect(
      within(screen.getByRole("navigation", { name: "客户端" })).getByRole(
        "button",
        { name: "Grok Build" },
      ),
    ).toHaveAttribute("aria-pressed", "true"),
  );
  await waitFor(() =>
    expect(mock.command).toHaveBeenCalledWith("get_gateway", {
      clientId: "grok",
    }),
  );
  expect(localStorage.getItem("vela.main.client")).toBe("grok");

  await user.click(await screen.findByRole("button", { name: "添加" }));
  const dialog = await screen.findByRole("dialog", { name: "添加供应商" });
  const protocol = within(dialog).getByLabelText("API 协议");
  expect(protocol).toHaveValue("responses");
  await user.selectOptions(protocol, "messages");
  await user.type(within(dialog).getByLabelText("名称"), "fixture grok provider");
  await user.type(
    within(dialog).getByLabelText("base_url"),
    "https://api.example.invalid/v1",
  );
  await user.type(within(dialog).getByLabelText("api_key"), "fixture-token");
  await user.click(within(dialog).getByRole("button", { name: "保存" }));

  await waitFor(() =>
    expect(mock.command).toHaveBeenCalledWith(
      "update_gateway",
      expect.objectContaining({
        clientId: "grok",
        expectedRevision: grok.revision,
        edit: {
          op: "saveGrokProvider",
          protocol: "messages",
          id: null,
          baseUrl: "https://api.example.invalid/v1",
          token: "fixture-token",
          name: "fixture grok provider",
        },
      }),
    ),
  );
});

it("offers the model list switch to Codex and Grok and saves it as an edit", async () => {
  const user = userEvent.setup();
  grok.modelCatalogError = "fixture catalog error";
  render(<Gateway notify={() => {}} />);

  await screen.findByRole("button", { name: "添加" });
  await user.click(screen.getByText("高级设置", { selector: "summary" }));
  const toggle = screen.getByRole("switch", { name: "接管模型列表" });
  expect(toggle).not.toBeChecked();
  expect(screen.queryByRole("combobox", { name: "列表来源" })).toBeNull();
  codex.modelCatalog = true;
  await user.click(toggle);
  await waitFor(() =>
    expect(mock.command).toHaveBeenCalledWith(
      "update_gateway",
      expect.objectContaining({
        clientId: "codex",
        edit: { op: "modelCatalog", enabled: true },
      }),
    ),
  );

  // Codex alone may list OpenAI's own catalog instead of the queue's models.
  const source = await screen.findByRole("combobox", { name: "列表来源" });
  expect(source).toHaveValue("queue");
  await user.selectOptions(source, "official");
  await waitFor(() =>
    expect(mock.command).toHaveBeenCalledWith(
      "update_gateway",
      expect.objectContaining({
        clientId: "codex",
        edit: { op: "modelCatalogSource", official: true },
      }),
    ),
  );

  grok.modelCatalog = true;
  await user.click(
    within(screen.getByRole("navigation", { name: "客户端" })).getByRole(
      "button",
      { name: "Grok Build" },
    ),
  );
  await waitFor(() =>
    expect(screen.getByRole("switch", { name: "接管模型列表" })).toBeChecked(),
  );
  expect(screen.getByText("fixture catalog error")).toBeInTheDocument();
  expect(screen.queryByRole("combobox", { name: "列表来源" })).toBeNull();
});
