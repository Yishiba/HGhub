# HGhub 远程配置发布分支

这个分支只存放 App 拉取的签名配置 `hghub-config.json`，与代码无关，不要合并到 main。

- 配置源文件、签名工具与发布步骤见 main 分支的 `docs/remote-config.md`。
- 文件由维护者私钥签名，App 只认验签通过且版本号更大的配置；手改这里的文件会导致验签失败、被 App 忽略。
- App 依次从 jsDelivr（cdn / fastly / testingcf）和 GitHub raw 拉取同一份文件。
