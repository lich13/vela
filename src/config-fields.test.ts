import { describe, expect, it } from "vitest";
import { codexFields, dshFields, grokFields } from "./config-fields";

const sensitive =
  /token|secret|password|passwd|authorization|credential|api_?key/i;

describe.each([
  ["Codex", codexFields],
  ["Grok", grokFields],
  ["DSH", dshFields],
])("%s 精选字段表", (_name, table) => {
  const fields = Object.values(table).flat();

  it("每个字段都有名称和路径", () => {
    for (const field of fields) {
      expect(field.label.trim()).not.toBe("");
      expect(field.path.length).toBeGreaterThan(0);
    }
  });

  it("下拉字段至少有两个选项", () => {
    for (const field of fields.filter((f) => f.kind === "select"))
      expect(field.options?.length ?? 0, field.label).toBeGreaterThanOrEqual(2);
  });

  it("路径不重复", () => {
    const paths = fields.map((f) => JSON.stringify(f.path));
    expect(new Set(paths).size).toBe(paths.length);
  });

  it("不收凭据类键", () => {
    for (const field of fields)
      for (const part of field.path)
        expect(String(part), field.label).not.toMatch(sensitive);
  });
});
