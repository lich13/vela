import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, it, vi } from "vitest";
vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);
import AntigravitySettings, {
  antigravityFields,
  antigravityGroups,
} from "./AntigravitySettings";

let draft = "";
function Host({ initial }: { initial: object }) {
  const [text, setText] = useState(JSON.stringify(initial, null, 2));
  draft = text;
  return <AntigravitySettings text={text} onChange={setText} />;
}

it("每个布尔字段都是开关，键不存在时也是", async () => {
  const user = userEvent.setup();
  render(<Host initial={{ userSettings: { enableTerminalSandbox: false } }} />);
  for (const group of antigravityGroups.slice(0, -1)) {
    await user.click(screen.getByRole("button", { name: group }));
    for (const field of antigravityFields[group])
      if (field.kind === "boolean")
        expect(screen.getByRole("switch", { name: field.label })).toBeVisible();
  }
  await user.click(screen.getByRole("button", { name: "执行与安全" }));
  await user.click(screen.getByRole("switch", { name: "终端沙盒" }));
  await user.click(screen.getByRole("button", { name: "远程与插件" }));
  await user.click(screen.getByRole("switch", { name: "远程控制" }));
  expect(JSON.parse(draft).userSettings).toEqual({
    enableTerminalSandbox: true,
    remoteControlEnabled: true,
  });
});

it("高级里的 0/1 取值是开关并沿用原来的写法，数量键仍是数字框", async () => {
  const user = userEvent.setup();
  render(
    <Host
      initial={{ userSettings: { legacyFlag: 1, quoted: "0", maxCount: 1 } }}
    />,
  );
  await user.click(screen.getByRole("button", { name: "高级" }));
  await user.click(screen.getByText("userSettings"));
  expect(
    screen.getByRole("spinbutton", { name: "userSettings.maxCount" }),
  ).toHaveValue(1);
  await user.click(
    screen.getByRole("switch", { name: "userSettings.legacyFlag" }),
  );
  await user.click(screen.getByRole("switch", { name: "userSettings.quoted" }));
  expect(JSON.parse(draft).userSettings).toEqual({
    legacyFlag: 0,
    quoted: "1",
    maxCount: 1,
  });
});

it("高级按顶层键分栏，嵌套对象直接展开，新建的对象自动选中", async () => {
  const user = userEvent.setup();
  const { container } = render(
    <Host
      initial={{ flat: "x", userSettings: { nested: { deep: { leaf: "v" } } } }}
    />,
  );
  await user.click(screen.getByRole("button", { name: "高级" }));
  const tabs = screen.getByRole("navigation", { name: "高级分栏" });
  expect(screen.getByLabelText("flat")).toHaveValue("x");
  expect(screen.queryByLabelText("userSettings.nested.deep.leaf")).toBeNull();
  await user.click(screen.getByRole("button", { name: "userSettings" }));
  expect(screen.getByLabelText("userSettings.nested.deep.leaf")).toHaveValue(
    "v",
  );
  expect(screen.queryByLabelText("flat")).toBeNull();
  expect(container.querySelector("details")).toBeNull();
  await user.click(screen.getByRole("button", { name: "顶层字段" }));
  await user.click(screen.getByRole("button", { name: "添加字段" }));
  await user.type(screen.getByLabelText("字段名称"), "extra");
  await user.selectOptions(screen.getByLabelText("字段类型"), "object");
  await user.click(screen.getByRole("button", { name: "添加" }));
  expect(JSON.parse(draft).extra).toEqual({});
  expect(tabs).toContainElement(
    screen.getByRole("button", { name: "extra", pressed: true }),
  );
});
