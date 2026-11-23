import { useState } from "react";
import { get, type Field, type Json } from "./claude-settings";
import { JsonNode, Rules, Settings, jsonEditing } from "./ClaudeSettings";

export const antigravityCliGroups = [
  "外观与交互",
  "执行与安全",
  "模型",
  "状态栏与标题",
  "高级",
] as const;

/** A script the CLI runs to draw a line of its own, with its on/off switch. */
const script = (key: "statusLine" | "title", section: string): Field[] => [
  {
    path: [key, "command"],
    label: "命令",
    kind: "text",
    hint: "通过 sh -c 运行，会话状态以 JSON 从标准输入传入",
    section,
  },
  {
    path: [key, "enabled"],
    label: "启用",
    kind: "boolean",
    initial: true,
    hint: "关闭后保留命令，改用内置显示",
    section,
  },
];

/**
 * From the published settings schema. `enableTelemetry` and `hooks` are not
 * read from this file, so they are not offered.
 */
export const antigravityCliFields: Record<string, Field[]> = {
  外观与交互: [
    {
      path: ["colorScheme"],
      label: "配色方案",
      kind: "select",
      initial: "terminal",
      options: [
        ["terminal", "跟随终端"],
        ["light", "浅色"],
        ["solarized light", "Solarized 浅色"],
        ["colorblind-friendly light", "色觉友好浅色"],
        ["dark", "深色"],
        ["solarized dark", "Solarized 深色"],
        ["colorblind-friendly dark", "色觉友好深色"],
        ["tokyo night", "Tokyo Night"],
      ],
    },
    {
      path: ["verbosity"],
      label: "过程详细程度",
      kind: "select",
      initial: "medium",
      options: [
        ["high", "详细"],
        ["medium", "适中"],
        ["low", "简略"],
      ],
    },
    {
      path: ["altScreenMode"],
      label: "全屏模式",
      kind: "select",
      initial: "default",
      options: [
        ["default", "仅全屏视图使用"],
        ["always", "始终"],
        ["never", "从不"],
      ],
    },
    {
      path: ["runningLightSpeed"],
      label: "动画速度",
      kind: "select",
      initial: "medium",
      options: [
        ["fast", "快"],
        ["medium", "中"],
        ["slow", "慢"],
        ["off", "关闭"],
      ],
    },
    {
      path: ["editorMode"],
      label: "输入框编辑模式",
      kind: "select",
      initial: "default",
      options: [
        ["default", "标准"],
        ["vim", "Vim"],
      ],
    },
    {
      path: ["vimInsertFirst"],
      label: "Vim 模式从插入态开始",
      kind: "boolean",
      initial: false,
    },
    {
      path: ["queuedMessages"],
      label: "任务进行中发送的消息",
      kind: "select",
      initial: "queue",
      options: [
        ["queue", "排队到本轮结束"],
        ["send-immediately", "立即发送"],
      ],
    },
    {
      path: ["pickerGrouping"],
      label: "会话选择列表",
      kind: "select",
      initial: "none",
      options: [
        ["none", "按时间排列"],
        ["grouped", "按工作区分组"],
      ],
    },
    {
      path: ["notifications"],
      label: "通知",
      kind: "boolean",
      initial: false,
      hint: "需要批准、回答或任务结束时响铃并发送桌面通知",
    },
    {
      path: ["showTips"],
      label: "显示使用提示",
      kind: "boolean",
      initial: true,
    },
    {
      path: ["showFeedbackSurvey"],
      label: "显示反馈问卷",
      kind: "boolean",
      initial: true,
    },
    {
      path: ["hideActiveItems"],
      label: "隐藏进行中的子代理与后台任务",
      kind: "boolean",
      initial: false,
    },
    {
      path: ["copyOnSelect"],
      label: "选中即复制",
      kind: "boolean",
      initial: true,
    },
    {
      path: ["clearScrollbackOnResize"],
      label: "重绘时清除终端回滚",
      kind: "boolean",
      initial: true,
    },
    {
      path: ["useTerminalCursor"],
      label: "使用终端自身的光标",
      kind: "boolean",
      initial: false,
    },
    {
      path: ["editor"],
      label: "外部编辑器",
      kind: "text",
      initial: "auto",
      hint: "auto 依次使用 $EDITOR 与系统默认",
    },
    {
      path: ["historySize"],
      label: "输入历史条数",
      kind: "number",
      initial: 2000,
      min: -1,
      hint: "-1 保留全部",
    },
  ],
  执行与安全: [
    {
      path: ["agentMode"],
      label: "启动时的执行模式",
      kind: "select",
      initial: "auto",
      options: [
        ["", "默认（编辑文件前询问）"],
        ["accept-edits", "自动接受编辑"],
        ["auto", "自动"],
        ["plan", "计划"],
      ],
    },
    {
      path: ["toolPermission"],
      label: "工具权限",
      kind: "select",
      initial: "request-review",
      options: [
        ["request-review", "执行前请求审核"],
        ["proceed-in-sandbox", "在沙盒内直接执行"],
        ["always-proceed", "始终直接执行"],
        ["strict", "严格"],
      ],
    },
    {
      path: ["artifactReviewPolicy"],
      label: "产物审核",
      kind: "select",
      initial: "asks-for-review",
      options: [
        ["asks-for-review", "等待审核"],
        ["agent-decides", "由代理决定"],
        ["always-proceed", "始终继续"],
      ],
    },
    {
      path: ["enableTerminalSandbox"],
      label: "终端沙盒",
      kind: "boolean",
      initial: false,
    },
    {
      path: ["allowNonWorkspaceAccess"],
      label: "工作区外文件访问",
      kind: "boolean",
      initial: false,
    },
    {
      path: ["disableSlashCommands"],
      label: "无界面模式不执行斜杠命令",
      kind: "boolean",
      initial: false,
      hint: "-p 模式下以 / 开头的提示原样发给模型",
    },
  ],
  模型: [
    {
      path: ["model"],
      label: "模型",
      kind: "text",
      hint: "/model 选择的模型标签；不可用时改用默认模型",
    },
    {
      path: ["modelProvider"],
      label: "模型来源",
      kind: "select",
      initial: "",
      options: [
        ["", "Antigravity 账号"],
        ["gemini", "Gemini API（GEMINI_API_KEY）"],
      ],
    },
    {
      path: ["useG1Credits"],
      label: "额度用尽后使用 AI 点数",
      kind: "boolean",
      initial: false,
    },
  ],
  状态栏与标题: [
    ...script("statusLine", "状态栏"),
    {
      path: ["statusLine", "padding"],
      label: "上方空行数",
      kind: "number",
      initial: 0,
      min: 0,
      section: "状态栏",
    },
    {
      path: ["statusLine", "stack_with_default"],
      label: "与内置状态栏同时显示",
      kind: "boolean",
      initial: false,
      section: "状态栏",
    },
    ...script("title", "窗口标题"),
  ],
};

export default function AntigravityCliSettings({
  text,
  onChange,
}: {
  text: string;
  onChange: (text: string) => void;
}) {
  const [group, setGroup] =
      useState<(typeof antigravityCliGroups)[number]>("外观与交互"),
    [error, setError] = useState("");
  let value: Json;
  try {
    value = get(text, [])!;
  } catch {
    return (
      <div className="banner error" role="alert">
        请在 JSON 编辑器中修正配置
      </div>
    );
  }
  const editing = jsonEditing(text, onChange, setError);
  return (
    <div className="claude-settings antigravity-settings">
      <nav className="settings-tabs" aria-label="Antigravity CLI 配置分组">
        {antigravityCliGroups.map((g) => (
          <button
            key={g}
            aria-pressed={group === g}
            onClick={() => setGroup(g)}
          >
            {g}
          </button>
        ))}
      </nav>
      <div className="settings-content">
        {error && (
          <div className="inline-error" role="alert">
            {error}
          </div>
        )}
        <Settings
          fields={antigravityCliFields[group] ?? []}
          editing={editing}
        />
        {group === "执行与安全" && (
          <Rules
            editing={editing}
            path={["permissions"]}
            tools={[
              "command",
              "read_file",
              "write_file",
              "read_url",
              "execute_url",
              "mcp",
            ]}
          />
        )}
        {group === "高级" && (
          <JsonNode
            path={[]}
            value={value}
            editing={editing}
            hidden={["enableTelemetry", "hooks"]}
          />
        )}
      </div>
    </div>
  );
}
