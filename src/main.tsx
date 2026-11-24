import React from "react";
import { createRoot } from "react-dom/client";
import { ready } from "./bridge";

const root = createRoot(document.getElementById("root")!);
const isQuickPanel =
  new URLSearchParams(location.search).get("panel") === "quick";

// The first render already knows the current instance, so pages read once.
void Promise.all([
  isQuickPanel ? import("./QuickEntry") : import("./AppEntry"),
  ready,
]).then(([{ default: Entry }]) => {
  root.render(
    <React.StrictMode>
      <Entry />
    </React.StrictMode>,
  );
});
