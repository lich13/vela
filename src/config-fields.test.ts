import { describe, expect, it } from "vitest";
import {
  codexFields,
  codexTables,
  credentialKey,
  dshFields,
  grokFields,
} from "./config-fields";

it("凭据键按词判断，与 Rust 的规则一致", () => {
  for (const key of [
    "env_key",
    "api_key",
    "apiKey",
    "http_headers",
    "env_http_headers",
    "experimental_bearer_token",
    "bearer_token_env_var",
    "client_secret_file",
    "Authorization",
    "accessToken",
    "credentials",
    "cookie_jar",
  ])
    expect(credentialKey(key), key).toBe(true);
  for (const key of [
    "key",
    "model_auto_compact_token_limit",
    "tool_output_token_limit",
    "cli_auth_credentials_store",
    "apiKeyEnv",
    "env_key_instructions",
    "max_tokens",
    "query_params",
  ])
    expect(credentialKey(key), key).toBe(false);
});

describe.each([
  ["Codex", codexFields],
  ["Grok", grokFields],
  ["DSH", dshFields],
  ...codexTables.map((spec): [string, typeof codexFields] => [
    `Codex ${spec.label}`,
    { [spec.label]: spec.fields },
  ]),
])("%s 精选字段表", (_name, table) => {
  const fields = Object.values(table).flat();

  it("每个字段都有名称和路径", () => {
    for (const field of fields) {
      expect(field.label.trim()).not.toBe("");
      expect(field.path.length).toBeGreaterThan(0);
    }
  });

  it("下拉字段有选项", () => {
    for (const field of fields.filter((f) => f.kind === "select"))
      expect(field.options?.length ?? 0, field.label).toBeGreaterThanOrEqual(1);
  });

  it("路径不重复", () => {
    const paths = fields.map((f) => JSON.stringify(f.path));
    expect(new Set(paths).size).toBe(paths.length);
  });

  it("不收凭据类键", () => {
    for (const field of fields)
      for (const part of field.path)
        expect(credentialKey(String(part)), field.label).toBe(false);
  });
});

it("Codex 精选字段包含数量上限键", () => {
  const paths = Object.values(codexFields)
    .flat()
    .map((f) => f.path.join("."));
  expect(paths).toContain("model_auto_compact_token_limit");
  expect(paths).toContain("tool_output_token_limit");
});

describe.each(codexTables)("具名子表 $label", (spec) => {
  it("首个字段取自模板，原文编辑的键都是凭据键", () => {
    const keys = spec.fields.map((f) => f.path.join("."));
    for (const key of spec.first) expect(keys).toContain(key);
    for (const [label, key] of spec.source) {
      expect(label.trim()).not.toBe("");
      expect(credentialKey(key), key).toBe(true);
    }
  });
});

it("模型供应商模板覆盖可调整的键", () => {
  const keys = codexTables[0].fields.map((f) => f.path.join("."));
  expect(keys).toEqual(
    expect.arrayContaining([
      "name",
      "base_url",
      "wire_api",
      "query_params",
      "request_max_retries",
      "stream_max_retries",
      "stream_idle_timeout_ms",
      "requires_openai_auth",
      "supports_websockets",
    ]),
  );
});
