import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import Home from "../src/pages/Home";
import Category from "../src/pages/Category";
import LinePicker from "../src/components/LinePicker";
import { AppProvider, useStore } from "../src/store";
import "../src/index.css";

type LineProbe = { url: string; ok: boolean; ms: number; error?: string };
type Request = { cmd: string; args: Record<string, unknown>; resolve: (value: unknown) => void; reject: (error: string) => void };
const pending: Request[] = [];
const fixture = {
  history: [] as { cmd: string; args: Record<string, unknown> }[],
  /** 线路相关 IPC（line_state / line_mirrors / line_probe / line_set） */
  lineCalls: [] as { cmd: string; args: Record<string, unknown> }[],
  debug: [] as string[],
  ready: false,
  setActive: (_active: boolean) => {},
  complete(cmd: string, key: string, result: "404" | "503" | "empty" | "item" | "timeout" | "proxy") {
    const index = pending.findIndex(r => r.cmd === cmd && (!key || r.args.cate === key));
    if (index < 0) throw new Error(`No pending request: ${cmd}/${key}`);
    const request = pending.splice(index, 1)[0];
    if (result === "404" || result === "503") {
      request.reject(`HTTP ${result} Gateway: <html><h1>nginx error</h1></html>`);
      return;
    }
    // 真机上抓到的原文：默认线路被 DNS 污染后连接超时
    if (result === "timeout") {
      request.reject("error sending request for url (https://huangguoai.com/api/edge/request) ← client error (Connect) ← operation timed out");
      return;
    }
    // 本机代理软件关了：不是线路的问题，不能引导切线路
    if (result === "proxy") {
      request.reject("error sending request for url (https://huangguoai.com/api/edge/request) ← client error (Connect) ← tunnel error: failed to create underlying connection");
      return;
    }
    request.resolve({
      items: result === "empty" ? [] : [{
        id: 991, title: `已加载${key || cmd}`, cover: "", tags: [], score: 0, hot: 0,
        episode_count: 1, total_episodes: 1, is_finished: true, is_original: 0,
        like_count: 0, comment_count: 0, collect_count: 0,
      }],
      pagination: { page: 1, pages: 1, size: 8, total: result === "empty" ? 0 : 1 },
    });
  },
};

// Preserve the real api.call error formatting and React pages; replace only Tauri IPC.
Object.assign(window, {
  apiLoadingFixture: fixture,
  __TAURI_INTERNALS__: { invoke(cmd: string, args: Record<string, unknown> = {}) {
    if (cmd === "app_boot") return Promise.resolve({ user: null, logged_in: false, base: "https://fixture.invalid" });
    if (cmd === "debug_log") { fixture.debug.push(String(args.msg)); return Promise.resolve(); }
    if (cmd.startsWith("line_")) {
      fixture.lineCalls.push({ cmd, args });
      if (cmd === "line_state") return Promise.resolve({
        base: "https://fixture.invalid", default: "https://fixture.invalid",
        fixed: [{ name: "固定线路 1", url: "https://fixture.invalid" }],
      });
      // 同一被墙域名下 3 个子域（引导检测只应抽 2 个）+ 1 个可用域名
      if (cmd === "line_mirrors") return Promise.resolve({ source: "fixture", items: [
        { url: "https://thu.dead.invalid", host: "thu.dead.invalid" },
        { url: "https://pku.dead.invalid", host: "pku.dead.invalid" },
        { url: "https://sjtu.dead.invalid", host: "sjtu.dead.invalid" },
        { url: "https://thu.alive.invalid", host: "thu.alive.invalid" },
      ] });
      if (cmd === "line_probe") return Promise.resolve((args.urls as string[]).map((url): LineProbe =>
        url.includes(".alive.") ? { url, ok: true, ms: 120 } : { url, ok: false, ms: 8000, error: "请求超时" }));
      if (cmd === "line_set") return Promise.resolve(args.url);
      if (cmd === "line_ready") return Promise.resolve({ base: "https://fixture.invalid", preferred: "https://fixture.invalid", auto: false, user: null, logged_in: false });
    }
    if (!["api_feed", "api_category", "api_posts"].includes(cmd)) return Promise.reject(`Unexpected IPC: ${cmd}`);
    fixture.history.push({ cmd, args });
    return new Promise((resolve, reject) => pending.push({ cmd, args, resolve, reject }));
  } },
});

/* 与 App.tsx 一致：全局线路面板 + 切换线路后首页按 lineEpoch 重新挂载 */
function HomeWithLines() {
  const { lineEpoch, linePicker, closeLinePicker } = useStore();
  return <>
    <Home key={lineEpoch} active bottomInset={0} />
    <LinePicker open={linePicker.open} guided={linePicker.guided} onClose={closeLinePicker} />
  </>;
}

function Fixture() {
  const [active, setActive] = useState(false);
  fixture.setActive = setActive;
  fixture.ready = true;
  const home = new URLSearchParams(location.search).get("page") === "home";
  return <AppProvider>
    <div style={{ position: "relative", width: 900, height: 1000 }}>
      {home ? <HomeWithLines /> : <Category wide active={active} />}
    </div>
  </AppProvider>;
}
createRoot(document.querySelector("#root")!).render(<React.StrictMode><Fixture /></React.StrictMode>);
