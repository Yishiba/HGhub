#!/usr/bin/env node
// 远程配置的密钥生成 / 签名 / 校验工具。说明见 docs/remote-config.md。
//
//   node scripts/remote-config.mjs keygen  [--key <私钥路径>]
//   node scripts/remote-config.mjs pubkey  [--key <私钥路径>]
//   node scripts/remote-config.mjs sign    <配置.json> --out <hghub-config.json> [--key <私钥路径>]
//   node scripts/remote-config.mjs verify  <hghub-config.json> [--pub <公钥 base64>]
//
// 私钥默认放在 ~/.config/hghub/remote-config-ed25519.pem，绝不能放进仓库。
// 签名格式必须与 src-tauri/src/remote_config.rs 的 `verify_envelope` 保持一致：
//   { "alg": "ed25519", "payload": base64(配置 JSON 字节), "sig": base64(ed25519(配置 JSON 字节)) }

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const DEFAULT_KEY = path.join(os.homedir(), ".config", "hghub", "remote-config-ed25519.pem");

function fail(msg) {
  console.error(`错误：${msg}`);
  process.exit(1);
}

function parseArgs(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      const v = argv[i + 1];
      if (v === undefined || v.startsWith("--")) fail(`参数 ${a} 缺少取值`);
      out[a.slice(2)] = v;
      i++;
    } else {
      out._.push(a);
    }
  }
  return out;
}

function rawPublicKey(keyObject) {
  // SPKI DER 的最后 32 字节就是 ed25519 原始公钥
  const der = crypto.createPublicKey(keyObject).export({ type: "spki", format: "der" });
  return der.subarray(der.length - 32);
}

function printPublicKey(raw) {
  console.log(`公钥（base64）：${raw.toString("base64")}`);
  const bytes = [...raw].map((b) => `0x${b.toString(16).padStart(2, "0")}`);
  const rows = [];
  for (let i = 0; i < bytes.length; i += 8) rows.push(`    ${bytes.slice(i, i + 8).join(", ")},`);
  console.log(`Rust 常量（写入 remote_config.rs 的 PUBLIC_KEY）：\n[\n${rows.join("\n")}\n]`);
}

function loadPrivateKey(keyPath) {
  if (!fs.existsSync(keyPath)) fail(`找不到私钥 ${keyPath}，先运行 keygen`);
  return crypto.createPrivateKey(fs.readFileSync(keyPath));
}

/** 只做最基本的形状检查；完整规则以 App 端（Rust）为准。 */
function checkPayload(cfg) {
  if (cfg?.schema !== 1) fail("schema 必须为 1");
  if (!Number.isSafeInteger(cfg.version) || cfg.version < 1) fail("version 必须是正整数，且每次发布都要比上一版大");
  if (!Array.isArray(cfg.lines) || cfg.lines.length === 0) fail("lines 不能为空");
  if (typeof cfg.default !== "string") fail("default 必须是字符串");
  if (!cfg.lines.some((l) => l?.url === cfg.default)) fail("default 必须是 lines 里的某一条 url");
  if (!Array.isArray(cfg.publish_pages)) fail("publish_pages 必须是数组");
}

const args = parseArgs(process.argv.slice(2));
const cmd = args._[0];
const keyPath = args.key || DEFAULT_KEY;

switch (cmd) {
  case "keygen": {
    if (fs.existsSync(keyPath)) fail(`${keyPath} 已存在，拒绝覆盖（覆盖后已发布的 App 将无法再验证新配置）`);
    fs.mkdirSync(path.dirname(keyPath), { recursive: true, mode: 0o700 });
    const { privateKey } = crypto.generateKeyPairSync("ed25519");
    fs.writeFileSync(keyPath, privateKey.export({ type: "pkcs8", format: "pem" }), { mode: 0o600, flag: "wx" });
    console.log(`私钥已写入 ${keyPath}（权限 600）。请离线备份，丢失后只能发新版 App 更换公钥。`);
    printPublicKey(rawPublicKey(privateKey));
    break;
  }
  case "pubkey": {
    printPublicKey(rawPublicKey(loadPrivateKey(keyPath)));
    break;
  }
  case "sign": {
    const src = args._[1];
    if (!src || !args.out) fail("用法：sign <配置.json> --out <hghub-config.json>");
    let cfg;
    try {
      cfg = JSON.parse(fs.readFileSync(src, "utf8"));
    } catch (e) {
      fail(`读取或解析 ${src} 失败：${e.message}`);
    }
    checkPayload(cfg);
    const payload = Buffer.from(JSON.stringify(cfg), "utf8");
    const sig = crypto.sign(null, payload, loadPrivateKey(keyPath));
    const envelope = { alg: "ed25519", payload: payload.toString("base64"), sig: sig.toString("base64") };
    fs.writeFileSync(args.out, `${JSON.stringify(envelope, null, 2)}\n`);
    console.log(`已签名 version=${cfg.version} → ${args.out}`);
    break;
  }
  case "verify": {
    const file = args._[1];
    if (!file) fail("用法：verify <hghub-config.json> [--pub <公钥 base64>]");
    const env = JSON.parse(fs.readFileSync(file, "utf8"));
    if (env.alg !== "ed25519") fail(`不支持的算法 ${env.alg}`);
    const raw = args.pub ? Buffer.from(args.pub, "base64") : rawPublicKey(loadPrivateKey(keyPath));
    if (raw.length !== 32) fail("公钥长度必须是 32 字节");
    const spki = Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), raw]);
    const pub = crypto.createPublicKey({ key: spki, format: "der", type: "spki" });
    const payload = Buffer.from(env.payload, "base64");
    if (!crypto.verify(null, payload, pub, Buffer.from(env.sig, "base64"))) fail("签名校验失败");
    const cfg = JSON.parse(payload.toString("utf8"));
    console.log(`签名有效：version=${cfg.version}，${cfg.lines.length} 条线路，默认 ${cfg.default}`);
    break;
  }
  default:
    fail("未知命令。可用：keygen / pubkey / sign / verify");
}
