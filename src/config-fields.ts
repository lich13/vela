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
 * Whether a key names a credential, by the same word rules as `sensitive()` in
 * `config_tree.rs`: Rust hides such values and refuses to patch them.
 */
export function credentialKey(key: string) {
  const words = key
    .replace(/([a-z\d])([A-Z])/g, "$1 $2")
    .toLowerCase()
    .split(/[^a-z\d]+/)
    .filter(Boolean);
  const last = words.at(-1) ?? "";
  return (
    words.some((word) =>
      /^(secrets?|passwords?|passwd|passphrase|authorization|bearer|cookies?)$/.test(
        word,
      ),
    ) ||
    /^(token|credentials?|apikey|headers?)$/.test(last) ||
    (/^keys?$/.test(last) && words.length > 1)
  );
}

/**
 * Tables that hold one sub-table per user-chosen name, such as
 * `[model_providers.<name>]`. Field paths are relative to the sub-table.
 */
export type NamedTables = {
  /** The tab name. */
  label: string;
  path: string[];
  /** Keys a new sub-table can start with; a TOML table exists once it has one. */
  first: string[];
  fields: Field[];
  /** Credential keys, named here and edited in the source: label, key. */
  source: [string, string][];
};

const summaries: [string, string][] = [
  ["auto", "自动"],
  ["concise", "简洁"],
  ["detailed", "详细"],
  ["none", "关闭"],
];
const verbosities: [string, string][] = [
  ["low", "低"],
  ["medium", "中"],
  ["high", "高"],
];
const approvals: [string, string][] = [
  ["on-request", "由模型决定何时询问"],
  ["never", "从不询问"],
];
const sandboxes: [string, string][] = [
  ["read-only", "只读"],
  ["workspace-write", "工作区可写"],
  ["danger-full-access", "完全访问（危险）"],
];
const searches: [string, string][] = [
  ["disabled", "关闭"],
  ["cached", "缓存"],
  ["indexed", "索引"],
  ["live", "实时"],
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
      options: summaries,
    },
    {
      label: "输出详略",
      path: ["model_verbosity"],
      kind: "select",
      options: verbosities,
    },
    {
      label: "上下文窗口（Token）",
      path: ["model_context_window"],
      kind: "number",
    },
    {
      label: "自动压缩阈值（Token）",
      path: ["model_auto_compact_token_limit"],
      kind: "number",
    },
    {
      label: "工具输出上限（Token）",
      path: ["tool_output_token_limit"],
      kind: "number",
      min: 0,
    },
  ],
  审批与沙箱: [
    {
      label: "审批策略",
      path: ["approval_policy"],
      kind: "select",
      options: approvals,
      hint: "细分审批（granular 表）请在结构树中编辑",
    },
    {
      label: "沙箱模式",
      path: ["sandbox_mode"],
      kind: "select",
      options: sandboxes,
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
      options: searches,
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
/** Keys follow `codex-rs/core/config.schema.json` of openai/codex. */
export const codexTables: NamedTables[] = [
  {
    label: "模型供应商",
    path: ["model_providers"],
    first: ["base_url"],
    fields: [
      { label: "显示名称", path: ["name"], kind: "text" },
      { label: "接口地址", path: ["base_url"], kind: "text" },
      {
        label: "接口协议",
        path: ["wire_api"],
        kind: "select",
        options: [["responses", "Responses API"]],
        initial: "responses",
      },
      {
        label: "使用 OpenAI 登录凭据",
        path: ["requires_openai_auth"],
        kind: "boolean",
        initial: false,
      },
      {
        label: "支持 WebSocket",
        path: ["supports_websockets"],
        kind: "boolean",
        initial: false,
      },
      {
        label: "支持独立网页搜索",
        path: ["supports_standalone_web_search"],
        kind: "boolean",
        initial: false,
      },
      {
        label: "请求重试次数",
        path: ["request_max_retries"],
        kind: "number",
        min: 0,
      },
      {
        label: "流重连次数",
        path: ["stream_max_retries"],
        kind: "number",
        min: 0,
      },
      {
        label: "流空闲超时（毫秒）",
        path: ["stream_idle_timeout_ms"],
        kind: "number",
        min: 0,
      },
      {
        label: "WebSocket 连接超时（毫秒）",
        path: ["websocket_connect_timeout_ms"],
        kind: "number",
        min: 0,
      },
      {
        label: "缺少密钥时的提示",
        path: ["env_key_instructions"],
        kind: "text",
      },
      {
        label: "查询参数",
        path: ["query_params"],
        kind: "map",
        hint: "附加到请求地址",
      },
    ],
    source: [
      ["API Key 环境变量", "env_key"],
      ["Bearer 令牌", "experimental_bearer_token"],
      ["请求头", "http_headers"],
      ["取自环境变量的请求头", "env_http_headers"],
    ],
  },
  {
    label: "配置档",
    path: ["profiles"],
    first: ["model"],
    fields: [
      { label: "模型", path: ["model"], kind: "text" },
      { label: "模型供应商", path: ["model_provider"], kind: "text" },
      { label: "推理强度", path: ["model_reasoning_effort"], kind: "text" },
      {
        label: "规划模式推理强度",
        path: ["plan_mode_reasoning_effort"],
        kind: "text",
      },
      {
        label: "推理摘要",
        path: ["model_reasoning_summary"],
        kind: "select",
        options: summaries,
      },
      {
        label: "输出详略",
        path: ["model_verbosity"],
        kind: "select",
        options: verbosities,
      },
      {
        label: "审批策略",
        path: ["approval_policy"],
        kind: "select",
        options: approvals,
      },
      {
        label: "沙箱模式",
        path: ["sandbox_mode"],
        kind: "select",
        options: sandboxes,
      },
      {
        label: "网页搜索",
        path: ["web_search"],
        kind: "select",
        options: searches,
      },
      { label: "服务层级", path: ["service_tier"], kind: "text" },
    ],
    source: [],
  },
  {
    label: "MCP 服务器",
    path: ["mcp_servers"],
    first: ["command", "url"],
    fields: [
      { label: "启用", path: ["enabled"], kind: "boolean" },
      { label: "启动失败时中止", path: ["required"], kind: "boolean" },
      { label: "启动命令", path: ["command"], kind: "text" },
      { label: "工作目录", path: ["cwd"], kind: "text" },
      { label: "服务地址", path: ["url"], kind: "text", hint: "HTTP 服务器" },
      {
        label: "启动超时（秒）",
        path: ["startup_timeout_sec"],
        kind: "number",
        min: 0,
      },
      {
        label: "工具超时（秒）",
        path: ["tool_timeout_sec"],
        kind: "number",
        min: 0,
      },
      {
        label: "启动就绪条件",
        path: ["startup_readiness"],
        kind: "select",
        options: [
          ["connection", "连接建立后"],
          ["catalog", "有缓存的工具目录即可"],
        ],
      },
      {
        label: "工具默认审批",
        path: ["default_tools_approval_mode"],
        kind: "select",
        options: [
          ["auto", "自动"],
          ["prompt", "每次询问"],
          ["writes", "写入时询问"],
          ["approve", "直接批准"],
        ],
      },
      {
        label: "并行调用工具",
        path: ["supports_parallel_tool_calls"],
        kind: "boolean",
      },
      { label: "命令参数", path: ["args"], kind: "list" },
      { label: "环境变量", path: ["env"], kind: "map" },
      { label: "仅启用的工具", path: ["enabled_tools"], kind: "list" },
      { label: "停用的工具", path: ["disabled_tools"], kind: "list" },
    ],
    source: [
      ["Bearer 令牌环境变量", "bearer_token_env_var"],
      ["请求头", "http_headers"],
      ["取自环境变量的请求头", "env_http_headers"],
    ],
  },
];
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
