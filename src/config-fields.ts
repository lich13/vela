import type { Field } from "./claude-settings";
import type { ConfigClientId } from "./types";

// One switcher order for every config view, including the DSH editor.
export const configClients: readonly ConfigClientId[] = [
  "codex",
  "claude",
  "antigravity",
  "grok",
  "dsh",
];

/**
 * Curated settings per file: group name → fields, in display order. Keys the
 * file does not hold yet can still be set from here; everything else in the
 * file is shown by the generic tree.
 */
export const codexFields: Record<string, Field[]> = {
  模型: [
    { label: "模型", path: ["model"], kind: "text" },
    {
      label: "审查模型",
      path: ["review_model"],
      kind: "text",
      hint: "/review 使用",
    },
    {
      label: "推理强度",
      path: ["model_reasoning_effort"],
      kind: "text",
      hint: "取模型支持的值，如 low、medium、high",
    },
    {
      label: "推理摘要",
      path: ["model_reasoning_summary"],
      kind: "select",
      options: [
        ["auto", "自动"],
        ["concise", "简洁"],
        ["detailed", "详细"],
        ["none", "关闭"],
      ],
    },
    {
      label: "输出详略",
      path: ["model_verbosity"],
      kind: "select",
      options: [
        ["low", "低"],
        ["medium", "中"],
        ["high", "高"],
      ],
    },
    {
      label: "上下文窗口（Token）",
      path: ["model_context_window"],
      kind: "number",
    },
  ],
  审批与沙箱: [
    {
      label: "审批策略",
      path: ["approval_policy"],
      kind: "select",
      options: [
        ["on-request", "由模型决定何时询问"],
        ["never", "从不询问"],
      ],
      hint: "细分审批（granular 表）请在结构树中编辑",
    },
    {
      label: "沙箱模式",
      path: ["sandbox_mode"],
      kind: "select",
      options: [
        ["read-only", "只读"],
        ["workspace-write", "工作区可写"],
        ["danger-full-access", "完全访问（危险）"],
      ],
    },
    {
      label: "工作区可写时允许联网",
      path: ["sandbox_workspace_write", "network_access"],
      kind: "boolean",
      initial: false,
    },
    {
      label: "额外可写目录",
      path: ["sandbox_workspace_write", "writable_roots"],
      kind: "list",
    },
  ],
  功能: [
    {
      label: "网页搜索",
      path: ["web_search"],
      kind: "select",
      options: [
        ["disabled", "关闭"],
        ["cached", "缓存"],
        ["indexed", "索引"],
        ["live", "实时"],
      ],
    },
    {
      label: "历史记录",
      path: ["history", "persistence"],
      kind: "select",
      options: [
        ["save-all", "全部保存"],
        ["none", "不保存"],
      ],
      initial: "save-all",
    },
    {
      label: "项目说明上限（字节）",
      path: ["project_doc_max_bytes"],
      kind: "number",
      initial: 32768,
      min: 0,
    },
    {
      label: "文件链接打开方式",
      path: ["file_opener"],
      kind: "select",
      options: [
        ["vscode", "VS Code"],
        ["vscode-insiders", "VS Code Insiders"],
        ["windsurf", "Windsurf"],
        ["cursor", "Cursor"],
        ["none", "不链接"],
      ],
    },
  ],
  界面: [
    {
      label: "隐藏推理过程",
      path: ["hide_agent_reasoning"],
      kind: "boolean",
      initial: false,
    },
    {
      label: "显示原始推理内容",
      path: ["show_raw_agent_reasoning"],
      kind: "boolean",
      initial: false,
    },
    {
      label: "动画",
      path: ["tui", "animations"],
      kind: "boolean",
      initial: true,
    },
    {
      label: "启动提示",
      path: ["tui", "show_tooltips"],
      kind: "boolean",
      initial: true,
    },
    {
      label: "失焦时自动回顾",
      path: ["tui", "auto_recap"],
      kind: "boolean",
      initial: true,
    },
    {
      label: "默认启用 Vim 模式",
      path: ["tui", "vim_mode_default"],
      kind: "boolean",
      initial: false,
    },
  ],
};
export const grokFields: Record<string, Field[]> = {
  权限与安全: [
    {
      label: "默认以自动权限模式启动",
      path: ["default_auto_mode"],
      kind: "boolean",
    },
    {
      label: "提供自动权限模式",
      path: ["auto_mode", "enabled"],
      kind: "boolean",
      initial: true,
    },
    {
      label: "始终批准工具调用",
      path: ["ui", "yolo"],
      kind: "boolean",
      hint: "等同 --yolo",
    },
    {
      label: "显示“始终允许”选项",
      path: ["ui", "remember_tool_approvals"],
      kind: "boolean",
    },
    {
      label: "沙箱配置",
      path: ["sandbox", "profile"],
      kind: "text",
      hint: "off、workspace、read-only、strict、minimal 或自定义名称",
    },
    {
      label: "沙箱启用时免 Bash 确认",
      path: ["sandbox", "auto_allow_bash"],
      kind: "boolean",
    },
    { label: "允许规则", path: ["permission", "allow"], kind: "list" },
    { label: "询问规则", path: ["permission", "ask"], kind: "list" },
    { label: "拒绝规则", path: ["permission", "deny"], kind: "list" },
  ],
  功能: [
    {
      label: "停用网页搜索",
      path: ["disable_web_search"],
      kind: "boolean",
    },
    {
      label: "记忆",
      path: ["memory", "enabled"],
      kind: "boolean",
      initial: false,
    },
    {
      label: "启用 /goal",
      path: ["goal", "enabled"],
      kind: "boolean",
      initial: true,
    },
    {
      label: "启动时检查更新",
      path: ["cli", "auto_update"],
      kind: "boolean",
    },
  ],
  会话: [
    {
      label: "自动压缩阈值（%）",
      path: ["session", "auto_compact_threshold_percent"],
      kind: "number",
      min: 0,
      max: 100,
    },
    {
      label: "向 Bash 注入 .envrc 变量",
      path: ["session", "load_envrc"],
      kind: "boolean",
    },
  ],
  界面: [
    {
      label: "主题",
      path: ["ui", "theme"],
      kind: "text",
      hint: "主题名，或 auto／system 跟随系统",
    },
    {
      label: "时间戳",
      path: ["ui", "timestamps"],
      kind: "select",
      options: [
        ["off", "不显示"],
        ["minimal", "仅提示与耗时行"],
        ["on", "每条消息"],
      ],
      initial: "on",
    },
    {
      label: "显示思考过程",
      path: ["ui", "show_thinking_blocks"],
      kind: "boolean",
      initial: true,
    },
    { label: "紧凑模式", path: ["ui", "compact_mode"], kind: "boolean" },
    {
      label: "默认多行输入",
      path: ["ui", "multiline_default"],
      kind: "boolean",
    },
    {
      label: "下一条提示建议",
      path: ["ui", "prompt_suggestions"],
      kind: "boolean",
    },
    {
      label: "回退前确认",
      path: ["ui", "confirm_before_rewind"],
      kind: "boolean",
    },
    {
      label: "按一次 Ctrl+C 退出",
      path: ["ui", "single_ctrl_c_quit"],
      kind: "boolean",
      initial: false,
    },
  ],
};
/** For `cordis.patch.yml`, profile and home alike. */
export const dshFields: Record<string, Field[]> = {};
