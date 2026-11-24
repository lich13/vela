import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, it, vi } from "vitest";
vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);
import ClaudeSettings from "./ClaudeSettings";

let draft = "";
function Host({ initial }: { initial: string }) {
  const [text, setText] = useState(initial);
  draft = text;
  return <ClaudeSettings text={text} onChange={setText} guarded={false} />;
}
async function open(initial: object, group: string) {
  const user = userEvent.setup();
  render(<Host initial={JSON.stringify(initial, null, 2)} />);
  await user.click(screen.getByRole("button", { name: group }));
  return user;
}
const env = () => JSON.parse(draft).env;

it("新增环境变量填 1 后变成开关，并沿用字符串写法", async () => {
  const user = await open({ env: {} }, "环境变量");
  await user.type(screen.getByLabelText("新环境变量名称"), "MY_FLAG");
  await user.click(screen.getByRole("button", { name: "添加" }));
  const input = screen.getByRole("textbox", { name: "MY_FLAG" });
  await user.type(input, "1");
  // The field stays while it is being typed into.
  expect(screen.queryByRole("switch", { name: "MY_FLAG" })).toBeNull();
  await user.tab();
  const toggle = screen.getByRole("switch", { name: "MY_FLAG" });
  expect(toggle).toBeChecked();
  await user.click(toggle);
  expect(env()).toEqual({ MY_FLAG: "0" });
});

it("手动改为输入框后不再跟随取值，数量键保持输入框", async () => {
  const user = await open(
    { env: { MY_FLAG: "true", RETRY_COUNT: "1" } },
    "环境变量",
  );
  expect(screen.getByRole("textbox", { name: "RETRY_COUNT" })).toHaveValue("1");
  await user.click(screen.getByRole("button", { name: "MY_FLAG 改为输入框" }));
  const input = screen.getByRole("textbox", { name: "MY_FLAG" });
  await user.clear(input);
  await user.type(input, "0");
  await user.tab();
  expect(screen.getByRole("textbox", { name: "MY_FLAG" })).toHaveValue("0");
  await user.click(screen.getByRole("button", { name: "MY_FLAG 改为开关" }));
  expect(screen.getByRole("switch", { name: "MY_FLAG" })).not.toBeChecked();
});

it("常用环境开关写入字符串 1，关闭写 0，恢复默认删除", async () => {
  const user = await open({ env: { DISABLE_AUTOUPDATER: "1" } }, "偏好");
  const telemetry = screen.getByRole("switch", { name: "环境开关 停用遥测" });
  expect(telemetry).not.toBeChecked();
  await user.click(telemetry);
  expect(env().DISABLE_TELEMETRY).toBe("1");
  const updater = screen.getByRole("switch", {
    name: "环境开关 停用自动更新",
  });
  expect(updater).toBeChecked();
  await user.click(updater);
  expect(env().DISABLE_AUTOUPDATER).toBe("0");
  await user.click(
    screen.getByRole("button", { name: "环境开关 停用遥测恢复默认" }),
  );
  expect(env()).toEqual({ DISABLE_AUTOUPDATER: "0" });
  await user.click(screen.getByRole("button", { name: "模型" }));
  await user.click(
    screen.getByRole("switch", { name: "云平台 使用 Amazon Bedrock" }),
  );
  expect(env().CLAUDE_CODE_USE_BEDROCK).toBe("1");
  // The same key is a switch in the variable list as well.
  await user.click(screen.getByRole("button", { name: "环境变量" }));
  expect(
    screen.getByRole("switch", { name: "CLAUDE_CODE_USE_BEDROCK" }),
  ).toBeChecked();
});
