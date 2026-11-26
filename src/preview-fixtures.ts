export const densePreview =
  new URLSearchParams(location.search).get("fixture") === "dense";
/**
 * The keys the dense fixture's gateways hand out, which its usage is counted
 * under too, and one deleted since that only the usage still names.
 */
export const previewKeys = {
  codex: [
    { id: "3f6a1c2e-7b4d-4e19-8a50-1d2c9e6b7f01", name: "团队 CI" },
    {
      id: "9c2e5d7a-1f3b-4c68-b0a4-6e8d2f1a3c02",
      name: "外部协作工具 · 跨区域研发与长名称显示",
    },
  ],
  claude: [{ id: "b7d4e9f0-2a6c-4b31-9e75-4c0a8d5e6f03", name: "团队 CI" }],
  deleted: "e1a8c3b5-6d2f-4a97-8c14-7b9f0e2d4a04",
};
