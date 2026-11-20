// Fault injection is local only; real React pages + api.call, mocked Tauri IPC.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { chromium } from "playwright-core";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = join(root, "test-results", "api-loading", new Date().toISOString().replace(/[:.]/g, "-"));
await mkdir(output, { recursive: true });
const report = { transport: "local browser fixture; Tauri IPC mocked", passed: [], external: [] };
let vite, browser, page;
try {
  vite = await createServer({ root, server: { host: "127.0.0.1", port: 0, strictPort: false } });
  await vite.listen();
  const origin = vite.resolvedUrls.local[0];
  browser = await chromium.launch({ headless: true, executablePath: process.env.BROWSER_EXECUTABLE || undefined });
  page = await browser.newPage({ viewport: { width: 1100, height: 1200 } });
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.route("**/*", route => {
    const url = route.request().url();
    if (new URL(url).origin === new URL(origin).origin) return route.continue();
    report.external.push(url); return route.abort();
  });
  const complete = (cmd, key, result) => page.evaluate(args => window.apiLoadingFixture.complete(...args), [cmd, key, result]);
  const calls = () => page.evaluate(() => window.apiLoadingFixture.history);
  const visible = async locator => { await locator.waitFor({ state: "visible" }); };

  await page.goto(`${origin}tests/api-loading.html?page=home`);
  await page.waitForFunction(() => window.apiLoadingFixture.history.length === 1);
  await complete("api_feed", "", "404");
  await visible(page.getByText("首页加载失败", { exact: true }));
  await visible(page.getByText(/服务器请求失败（HTTP 404）/));
  assert.equal(await page.locator(".spinner").count(), 0, "Failed home must stop spinning");
  assert(!await page.locator("body").innerText().then(s => s.includes("<html>")), "Hide gateway HTML from UI");
  assert.equal(await page.getByRole("button", { name: "切换线路", exact: true }).count(), 0, "HTTP errors must not suggest switching lines");
  await page.getByRole("button", { name: "重试", exact: true }).click();
  await page.waitForFunction(() => window.apiLoadingFixture.history.length === 2);
  await complete("api_feed", "", "empty");
  await visible(page.getByText("暂无视频", { exact: true }));
  assert.equal(await page.locator(".spinner").count(), 0);
  report.passed.push("Home: 404 ends loading, readable error, explicit retry, empty result");

  // —— 线路连不上 → 引导切换线路 ——
  const lineCalls = () => page.evaluate(() => window.apiLoadingFixture.lineCalls);
  const switchBtn = page.getByRole("button", { name: "切换线路", exact: true });
  await page.goto(`${origin}tests/api-loading.html?page=home`);
  await page.waitForFunction(() => window.apiLoadingFixture.history.length === 1);
  // 反向：本机代理错误不是线路问题，只给「重试」
  await complete("api_feed", "", "proxy");
  await visible(page.getByText("首页加载失败", { exact: true }));
  assert.equal(await switchBtn.count(), 0, "Proxy errors must not suggest switching lines");
  await page.getByRole("button", { name: "重试", exact: true }).click();
  await page.waitForFunction(() => window.apiLoadingFixture.history.length === 2);
  // 正向：连接超时（真机原文）→ 主按钮「切换线路」，保留「重试」
  await complete("api_feed", "", "timeout");
  await visible(switchBtn);
  await visible(page.getByText(/当前线路连不上.*operation timed out/));
  await visible(page.getByRole("button", { name: "重试", exact: true }));
  assert(!await page.locator("body").innerText().then(s => s.includes("error sending request")), "Hide raw reqwest chain from UI");
  await switchBtn.click();
  await page.waitForFunction(() => window.apiLoadingFixture.lineCalls.some(c => c.cmd === "line_probe"));
  const probed = (await lineCalls()).find(c => c.cmd === "line_probe").args.urls;
  assert.deepEqual(probed, [
    "https://fixture.invalid", "https://thu.dead.invalid", "https://pku.dead.invalid", "https://thu.alive.invalid",
  ], "Guided mode probes fixed lines + at most 2 mirrors per domain");
  assert.equal((await lineCalls()).filter(c => c.cmd === "line_mirrors").length, 1, "Guided mode refreshes mirrors");
  await visible(page.getByText(/找到 1 条能连上的线路/));
  const order = await page.locator("button").allInnerTexts();
  const at = host => order.findIndex(t => t.startsWith(host));
  assert(at("thu.alive.invalid") >= 0 && at("thu.alive.invalid") < at("thu.dead.invalid"), "Reachable mirror sorts first");
  await page.getByRole("button", { name: /^thu\.alive\.invalid/ }).click();
  await page.waitForFunction(() => window.apiLoadingFixture.lineCalls.some(c => c.cmd === "line_set"));
  assert.equal((await lineCalls()).find(c => c.cmd === "line_set").args.url, "https://thu.alive.invalid");
  await page.getByText(/找到 1 条能连上的线路/).waitFor({ state: "hidden" });
  // 切换后首页重新挂载并重新加载
  await page.waitForFunction(() => window.apiLoadingFixture.history.length === 3);
  await complete("api_feed", "", "empty");
  await visible(page.getByText("暂无视频", { exact: true }));
  report.passed.push("Home: line-down errors offer guided switch (proxy errors don't), guided picker refreshes + samples mirrors, sorts reachable first, switching reloads home");

  await page.goto(`${origin}tests/api-loading.html`);
  await page.waitForFunction(() => window.apiLoadingFixture.ready);
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  assert.equal((await calls()).length, 0, "Hidden categories must not prefetch");
  await page.evaluate(() => window.apiLoadingFixture.setActive(true));
  await page.waitForFunction(() => window.apiLoadingFixture.history.length === 5);
  await complete("api_category", "ai-duanju", "item");
  await visible(page.getByText("已加载ai-duanju", { exact: true }));
  await visible(page.getByRole("region", { name: "AI魔改", exact: true }).getByText("加载中…", { exact: true }));
  await complete("api_category", "ai-huanlian", "empty");
  await visible(page.getByRole("region", { name: "AI换脸", exact: true }).getByText("暂无内容", { exact: true }));
  await complete("api_posts", "", "item");
  await visible(page.getByText("已加载api_posts", { exact: true }));
  await complete("api_category", "ai-manju", "503");
  await page.getByRole("button", { name: "重试AI成人漫剧", exact: true }).click();
  await page.waitForFunction(() => window.apiLoadingFixture.history.length === 6);
  await complete("api_category", "ai-manju", "item");
  await visible(page.getByText("已加载ai-manju", { exact: true }));
  assert.equal((await calls()).filter(r => r.args.cate === "ai-duanju").length, 1, "Retry must only reload the failed channel");
  await complete("api_category", "ai-mogai", "404");
  await visible(page.getByRole("button", { name: "重试AI魔改", exact: true }));
  await page.evaluate(() => window.apiLoadingFixture.setActive(false));
  await page.evaluate(() => window.apiLoadingFixture.setActive(true));
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(resolve)));
  assert.equal((await calls()).length, 6, "Revisiting must preserve previews without reloading all channels");
  report.passed.push("Category: defer hidden loading, show successful rows/posts before slow row, independent error/retry, preserve data on revisit");
  assert.deepEqual(errors, [], "No page errors");
  assert.deepEqual(report.external, [], "No upstream traffic");
  await page.screenshot({ path: join(output, "final.png"), fullPage: true });
  console.log(JSON.stringify(report, null, 2));
} catch (error) {
  report.error = String(error.stack || error);
  await page?.screenshot({ path: join(output, "failure.png"), fullPage: true }).catch(() => {});
  console.error(report.error);
  process.exitCode = 1;
} finally {
  await writeFile(join(output, "report.json"), JSON.stringify(report, null, 2));
  await browser?.close();
  await vite?.close();
  console.log(`Artifacts: ${output}`);
}
