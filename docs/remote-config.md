# 远程配置（线路与发布页）

上游域名会变、会被 DNS 污染，但每次都发新版 App 太重。远程配置把**固定线路清单、默认线路、镜像发布页清单**做成一份带签名的 JSON，托管在公开静态地址上，不需要服务端。实现见 `src-tauri/src/remote_config.rs`。

## 工作方式

- **签名**：配置用维护者的 ed25519 私钥签名，App 内置对应公钥，只认验签通过的内容。托管方（jsDelivr / GitHub）只负责搬运，改不了内容。
- **托管**：同一份 `hghub-config.json` 放在本 fork 的 `remote-config` 分支（孤立分支，只有这一个文件和说明），App 依次尝试：
  1. `https://cdn.jsdelivr.net/gh/Yishiba/HGhub@remote-config/hghub-config.json`
  2. `https://fastly.jsdelivr.net/gh/Yishiba/HGhub@remote-config/hghub-config.json`
  3. `https://testingcf.jsdelivr.net/gh/Yishiba/HGhub@remote-config/hghub-config.json`
  4. `https://raw.githubusercontent.com/Yishiba/HGhub/remote-config/hghub-config.json`
- **加载**：启动时先读本地缓存（应用数据目录 `remote_config.json`，每次都重新验签），再在后台拉取；**版本号必须大于当前版本**才会采用并落盘，旧文件回放无效。拿不到任何有效配置时回退到内置默认值（与引入远程配置之前一致）。
- **拉取顺序**：依次尝试各托管源，拿到比当前更新的版本就停。某个源版本不比当前新（jsDelivr 常缓存旧版）时会继续试下一个源，否则 CDN 的旧缓存会挡住 GitHub raw 上已经发布的新版本。
- **生效时机**：线路清单、发布页清单在下次打开「线路」时生效；**默认线路只在启动时读取**，所以新默认线路下次启动才生效。用户手动选过线路（存在 `line.json`）就始终以用户选择为准。

## 冷启动自动选线路

实现见 `src-tauri/src/line_select.rs`。

- **首选线路**是用户手动选过的线路（`line.json`），没选过就是配置里的默认线路；能用就始终用它。
- 首选线路连不上时：先试上次自动选中的线路（`line_auto.json`），再从固定线路 + 发布页最新镜像（每个域名抽 2 个，最多 12 个）里并发检测，最先成功的胜出。
- 自动选中的线路只改当前线路，**不覆盖用户的手动选择、不清登录态**（会话与站点绑定：镜像上是游客，首选线路恢复后登录态还在）。首选线路恢复后清掉自动记录，下次启动回到首选线路。
- 检测走真实接口（`/api/videos?page=1&size=1` 返回业务成功才算），线路面板的「检测」也是同一套：`huangguoai.ai` 首页 200 但接口 404，只看首页会误判为可用。
- 业务请求、登录相关请求在 Rust 侧等选线路结束（最多 25 秒）再发出；前端通过 `line_ready` 拿最终线路与登录态，自动换线时提示一次。
- 全部连不上时保持首选线路，首页报错页仍提供「切换线路」引导。

## 配置发布记录

| 版本 | 日期 | 内容 |
|---|---|---|
| 1 | 2026-11-16 | 线路沿用内置值；发布页优先 `huang-guo.pages.dev`（`huangguoai.pages.dev` 在国内 DNS 下被污染） |
| 2 | 2026-11-20 | 去掉 `huangguoai.ai`：已换成新的加密接口协议，`/api/edge/request` 返回 404，对当前客户端不可用 |

## 安全边界

- 远程配置**不能扩大登录信任范围**：账号密码只发往 `net::trusted_auth_origin` 写死的 `huangguoai.com` / `huangguoai.ai`。配置里新增的固定线路，在新版 App 把它加入信任名单之前只能游客浏览。
- 内容即使签名正确也要过校验：线路必须是纯 HTTPS origin（不允许路径、认证信息、内网地址）、默认线路必须在清单里、线路最多 10 条、发布页最多 5 个且必须是 HTTPS。任何一项不通过整份拒绝，不做部分采用。
- 托管地址的域名里不要带上游站点的关键词：实测 `huangguoai.pages.dev` 在国内 DNS 下会被污染，`huang-guo.pages.dev`、jsDelivr、GitHub raw 正常（2026-11-16 测）。

## 私钥

- 位置：维护者本机 `~/.config/hghub/remote-config-ed25519.pem`（权限 600），**不在仓库里，也不要放进仓库**。
- 请离线备份。丢失后无法再发布新配置，只能发新版 App 更换 `remote_config.rs` 里的 `PUBLIC_KEY`；泄露则任何人都能向所有用户下发线路清单，同样需要发版换钥。
- 公钥：`K0lAnULDsGTtkXTSYqbElaO4emnQm4gsf4DGyOZ4vow=`（base64）。

## 发布新配置

配置源文件是 main 分支的 `remote-config/config.json`，改动走正常代码评审。

1. 修改 `remote-config/config.json`，并把 `version` 加 1（必须比线上版本大，否则 App 不会采用）。
2. 签名并检查：

   ```bash
   node scripts/remote-config.mjs sign remote-config/config.json --out /tmp/hghub-config.json
   node scripts/remote-config.mjs verify /tmp/hghub-config.json --pub K0lAnULDsGTtkXTSYqbElaO4emnQm4gsf4DGyOZ4vow=
   ```

3. 发布到 `remote-config` 分支：

   ```bash
   git worktree add /tmp/hghub-remote-config remote-config
   cp /tmp/hghub-config.json /tmp/hghub-remote-config/hghub-config.json
   git -C /tmp/hghub-remote-config commit -am "chore(remote-config): 发布远程配置 version N"
   git -C /tmp/hghub-remote-config push origin remote-config
   git worktree remove /tmp/hghub-remote-config
   ```

4. jsDelivr 对分支有缓存（通常最长 12 小时），想立即生效可以请求一次刷新：`https://purge.jsdelivr.net/gh/Yishiba/HGhub@remote-config/hghub-config.json`。GitHub raw 约 5 分钟生效。
5. 验证各托管源都能拿到新版本：

   ```bash
   cd src-tauri && cargo test --lib remote_config::tests::sources_live -- --ignored --nocapture
   ```

## 测试

- `fixtures/remote-config-test.json` 是用**测试密钥**（与正式密钥无关）经 `scripts/remote-config.mjs sign` 签出的样例，单测用它保证 Node 签名与 Rust 验签格式一致。
- 单测覆盖：验签通过 / 篡改内容 / 篡改签名 / 换公钥 / 超大文件、各类非法内容（含签名正确但内容不安全）、防回滚。
