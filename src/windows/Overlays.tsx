import { useEffect, useRef, useState } from "react";
import { api, inTauri, onEvent } from "../api";
import { QuickPanel } from "../components/QuickPanel";
import { useAccounts, useSettings } from "../lib/hooks";

/** 卡片上下的透明边距（给阴影），和 styles.css 的 .quick-wrap 一致。 */
const INSET_Y: Record<string, number> = { edge: 8 + 22, tray: 6 + 22 };

/** 量卡片内容的高度，让窗口贴合内容。量的是内容本身（不受窗口大小限制），
 *  再加上卡片的内边距 28、边框 2 和上下给阴影留的透明边距。 */
function useFitHeight(label: string, active: boolean) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el || !active || !inTauri) return;
    let last = 0;
    const report = () => {
      const h = Math.ceil(el.offsetHeight + 28 + 2 + INSET_Y[label]);
      if (Math.abs(h - last) > 1) {
        last = h;
        api.setPanelHeight(label, h).catch(() => {});
      }
    };
    report();
    const ro = new ResizeObserver(report);
    ro.observe(el);
    return () => ro.disconnect();
  }, [label, active]);
  return ref;
}

/** 屏幕边缘：收起时是一根细条，光标贴边后由后端把窗口放大并发来 edge-expanded。 */
export function EdgeWindow() {
  const [expanded, setExpanded] = useState(!inTauri);
  const [settings] = useSettings();
  const accounts = useAccounts();
  const fit = useFitHeight("edge", expanded);

  useEffect(() => {
    document.body.classList.add("overlay");
    return onEvent<boolean>("edge-expanded", setExpanded);
  }, []);

  if (!expanded) {
    // 细条颜色提示额度：任一额度用到 90% 变红，70% 变黄
    const max = Math.max(0, ...(accounts ?? []).flatMap((a) => a.windows.filter((w) => !w.expired).map((w) => w.usedPercent)));
    return (
      <div className={"strip" + (max >= 90 ? " alert" : max >= 70 ? " warn" : "")}>
        <i />
      </div>
    );
  }

  const side = settings?.edgeSide === "left" ? "edge-left" : "edge-right";
  return (
    <div className={"quick-wrap " + side} style={!inTauri ? { width: 256 } : undefined}>
      <div className="quick enter">
        <div ref={fit} style={{ display: "flex", flexDirection: "column" }}>
          <QuickPanel onOpen={(page) => api.openMain(page)} />
        </div>
      </div>
    </div>
  );
}

/** 点菜单栏图标弹出的卡片。 */
export function TrayWindow() {
  const [key, setKey] = useState(0);
  const fit = useFitHeight("tray", true);
  useEffect(() => {
    document.body.classList.add("overlay");
    return onEvent("tray-shown", () => setKey((k) => k + 1));
  }, []);
  return (
    <div className="quick-wrap" style={!inTauri ? { width: 256 } : undefined}>
      <div className="quick enter" key={key}>
        <div ref={fit} style={{ display: "flex", flexDirection: "column" }}>
          <QuickPanel onOpen={(page) => api.openMain(page)} />
        </div>
      </div>
    </div>
  );
}
