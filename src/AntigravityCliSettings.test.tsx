import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it } from "vitest";
import AntigravityCliSettings, {
  antigravityCliFields,
} from "./AntigravityCliSettings";

const source = `{
  "agentMode": "auto",
  "notifications": false,
  "enableTelemetry": true,
  "hooks": {},
  "futureKey": { "kept": 1 }
}
`;
let text = source;
function Form({ initial = source }: { initial?: string }) {
  const [draft, setDraft] = useState(initial);
  text = draft;
  return <AntigravityCliSettings text={draft} onChange={setDraft} />;
}
const tab = (name: string) =>
  fireEvent.click(screen.getByRole("button", { name }));

afterEach(cleanup);

describe("Antigravity CLI 配置表单", () => {
  it("开关与下拉只改动所选字段，保留未知字段和原有排版", () => {
    render(<Form />);
    fireEvent.click(screen.getByRole("switch", { name: "通知" }));
    expect(text).toContain('"notifications": true');
    // Absent keys show the CLI's default and write an explicit value.
    const tips = screen.getByRole("switch", { name: "显示使用提示" });
    expect(tips).toBeChecked();
    fireEvent.click(tips);
    expect(text).toContain('"showTips": false');
    tab("执行与安全");
    fireEvent.change(screen.getByRole("combobox", { name: "工具权限" }), {
      target: { value: "strict" },
    });
    expect(text).toContain('"toolPermission": "strict"');
    fireEvent.change(
      screen.getByRole("combobox", { name: "启动时的执行模式" }),
      { target: { value: "__default__" } },
    );
    expect(text).not.toContain("agentMode");
    expect(text).toContain('"futureKey": { "kept": 1 }');
  });

  it("状态栏的命令与开关写在同一张表里", () => {
    render(<Form initial={"{}\n"} />);
    tab("状态栏与标题");
    fireEvent.change(screen.getByRole("textbox", { name: "状态栏 命令" }), {
      target: { value: "~/statusline.sh" },
    });
    fireEvent.click(screen.getByRole("switch", { name: "状态栏 启用" }));
    expect(JSON.parse(text)).toEqual({
      statusLine: { command: "~/statusline.sh", enabled: false },
    });
  });

  it("权限规则可以添加和删除", () => {
    render(<Form initial={"{}\n"} />);
    tab("执行与安全");
    fireEvent.change(screen.getByRole("combobox", { name: "规则工具" }), {
      target: { value: "read_url" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "匹配内容" }), {
      target: { value: "example.invalid" },
    });
    fireEvent.change(screen.getByRole("combobox", { name: "规则处理方式" }), {
      target: { value: "deny" },
    });
    fireEvent.click(screen.getByRole("button", { name: "添加" }));
    expect(JSON.parse(text)).toEqual({
      permissions: { deny: ["read_url(example.invalid)"] },
    });
  });

  it("不提供 CLI 不读取的字段，其余未知字段在高级里", () => {
    render(<Form />);
    const offered = Object.values(antigravityCliFields)
      .flat()
      .map((field) => field.path[0]);
    expect(offered).not.toContain("enableTelemetry");
    expect(offered).not.toContain("hooks");
    tab("高级");
    expect(screen.getByText("futureKey")).toBeInTheDocument();
    expect(screen.queryByText("enableTelemetry")).not.toBeInTheDocument();
    expect(screen.queryByText("hooks")).not.toBeInTheDocument();
  });

  it("JSON 无效时提示改用 JSON 编辑器", () => {
    render(<Form initial={'{ "agentMode": '} />);
    expect(screen.getByRole("alert")).toHaveTextContent(
      "请在 JSON 编辑器中修正配置",
    );
  });
});
