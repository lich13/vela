import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ClientId, GatewayState } from "./types";

import { mock } from "./test-bridge";
vi.mock("./bridge", async () => (await import("./test-bridge")).unfrozenBridge);

vi.mock("./confirmation", () => ({ confirmAction: mock.confirm }));

import Gateway from "./Gateway";
import { gatewayDemo, grokGatewayDemo } from "./gateway-preview";

let codex: GatewayState;
let grok: GatewayState;

beforeEach(() => {
  localStorage.clear();
  codex = { ...structuredClone(gatewayDemo), providers: [] };
  grok = { ...structuredClone(grokGatewayDemo), providers: [] };
  mock.command.mockReset();
  mock.confirm.mockReset().mockResolvedValue(true);
  mock.command.mockImplementation(
    async (name: string, args?: { clientId?: ClientId }) => {
      const state = args?.clientId === "grok" ? grok : codex;
      if (
        name === "get_gateway" ||
        name === "update_gateway" ||
        name === "update_codex_catalog"
      )
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
          bufferCompleteResponse: false,
          protocol: "messages",
          id: null,
          baseUrl: "https://api.example.invalid/v1",
          token: "fixture-token",
          name: "fixture grok provider",
          // Only a Responses provider is offered the web searches.
          takeWebSearch: false,
          webSearchModel: "",
          webSearchEffort: "",
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
  codex.modelCatalogOfficial = true;
  codex.modelCatalogOfficialStatus = {
    models: 14,
    version: "1a2b3c4d",
    updatedAt: 1,
  };
  codex.modelCatalogOfficialError = "fixture download error";
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
  // What is held, since when, and the last download's error; then a download by hand.
  expect(
    await screen.findByText(
      /^14 个模型 · 版本 1a2b3c4d · 最近更新 .+ · fixture download error$/,
    ),
  ).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "立即更新" }));
  await waitFor(() =>
    expect(mock.command).toHaveBeenCalledWith(
      "update_codex_catalog",
      undefined,
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
