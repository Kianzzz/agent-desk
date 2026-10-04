import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { api, onEvent, windowLabel } from "./api";
import { MainApp } from "./app/MainApp";
import { EdgeWindow, TrayWindow } from "./windows/Overlays";
import "./styles.css";

/** 先用上次记下的外观，避免窗口刚打开时闪一下另一种颜色；读到设置后再校正。 */
function applyTheme(theme: string) {
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem("theme", theme);
  } catch {
    /* 存不了就算了 */
  }
}
try {
  document.documentElement.dataset.theme = localStorage.getItem("theme") || "dark";
} catch {
  document.documentElement.dataset.theme = "dark";
}
const syncTheme = () => api.getSettings().then((s) => applyTheme(s.theme ?? "dark"), () => {});
syncTheme();
onEvent("settings-updated", syncTheme);

// 透明的浮层窗口要在第一帧之前就去掉底色，否则会先闪一下方框
{
  const label = (window as unknown as { __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: string } } } })
    .__TAURI_INTERNALS__?.metadata?.currentWindow?.label;
  if (label === "edge" || label === "tray" || /[?&]w=(edge|tray)/.test(location.search)) {
    document.documentElement.classList.add("overlay");
  }
}

function Root() {
  const [label, setLabel] = useState<string | null>(null);
  useEffect(() => {
    windowLabel().then(setLabel);
  }, []);
  if (label === null) return null;
  if (label === "edge") return <EdgeWindow />;
  if (label === "tray") return <TrayWindow />;
  return <MainApp />;
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Root />
  </StrictMode>,
);
