// Browser-preview stand-ins for the Antigravity CLI settings and structure-tree commands.
import type { ConfigDocument } from "./types";

let cli: ConfigDocument & { missing: boolean } = {
  clientId: "antigravity",
  guarded: false,
  canRestore: false,
  missing: false,
  path: "~/.gemini/antigravity-cli/settings.json",
  revision: "preview-antigravity-cli",
  text:
    JSON.stringify(
      {
        agentMode: "auto",
        notifications: true,
        verbosity: "medium",
        permissions: { deny: ["command(rm -rf *)"] },
        statusLine: { type: "command", command: "~/statusline.sh", enabled: true },
      },
      null,
      2,
    ) + "\n",
};
let previous: string | null = null;

export function run(name: string, args: Record<string, unknown>): unknown {
  switch (name) {
    case "read_antigravity_cli_config":
      return structuredClone(cli);
    case "validate_antigravity_cli_config": {
      const parsed = JSON.parse(String(args.text));
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
        throw { code: "JSON", message: "配置根节点必须是对象" };
      return;
    }
    case "read_previous_antigravity_cli_config":
      if (previous === null)
        throw { code: "RESTORE", message: "没有可恢复的上次配置" };
      return previous;
    case "save_antigravity_cli_config":
      run("validate_antigravity_cli_config", args);
      if (args.expectedRevision !== cli.revision)
        throw { code: "CONFLICT", message: "配置已被外部修改，草稿已保留" };
      previous = cli.text;
      cli = {
        ...cli,
        text: String(args.text),
        revision: crypto.randomUUID(),
        canRestore: true,
      };
      return structuredClone(cli);
    default:
      // TOML and YAML are parsed by Rust only; the preview keeps the source editor.
      throw {
        code: "CONFIG",
        message: "浏览器预览不解析 TOML／YAML，请在桌面客户端中使用可视化",
      };
  }
}
