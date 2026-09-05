import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { installTheme } from "./theme";

// Before React mounts, so the first paint is already the right palette. The
// stylesheet carries a prefers-color-scheme query, so "system" is correct without
// any JavaScript having run.
installTheme();

const style = document.createElement("style");
style.textContent = `html, body, #root { margin: 0; height: 100%; } body { background: var(--page-bg); } * { box-sizing: border-box; }`;
document.head.appendChild(style);

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
