//! 远程配置：带签名的线路与发布页清单，不需要自建服务端。
//!
//! - 配置文件是一个 JSON 信封 `{ alg, payload, sig }`：`payload` 是配置 JSON 的 base64，
//!   `sig` 是对这段字节的 ed25519 签名。只认 [`PUBLIC_KEY`] 验签通过的内容，所以托管在哪里
//!   都无所谓（jsDelivr / GitHub raw 都只是搬运工，改不了内容）。签名工具见 `scripts/remote-config.mjs`。
//! - 启动时先读本地缓存（上次验签通过的信封，离线 / 全部源被墙也能用），然后后台按 [`SOURCES`]
//!   顺序拉取；版本号必须**大于**当前版本才会采用并落盘（防回滚）。
//! - 线路清单、发布页清单在下次读取时即生效；**默认线路只在启动时读取**，避免会话中途悄悄换站点。
//!   用户手动选过线路（存在 `line.json`）就始终以用户选择为准。
//! - 远程配置**不能**扩大登录信任范围：账号密码只发往 `net::trusted_auth_origin` 写死的站点。
//!   配置里新增的固定线路在新版 App 写入信任名单之前，只能游客浏览。
//! - 拿不到任何有效配置时，回退到下面的内置默认值（与引入远程配置之前的行为一致）。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

/// 正式签名公钥。私钥在维护者本机 `~/.config/hghub/remote-config-ed25519.pem`，不在仓库里。
const PUBLIC_KEY: [u8; 32] = [
    0x2b, 0x49, 0x40, 0x9d, 0x42, 0xc3, 0xb0, 0x64, //
    0xed, 0x91, 0x74, 0xd2, 0x62, 0xa6, 0xc4, 0x95, //
    0xa3, 0xb8, 0x7a, 0x69, 0xd0, 0x9b, 0x88, 0x2c, //
    0x7f, 0x80, 0xc6, 0xc8, 0xe6, 0x78, 0xbe, 0x8c, //
];

/// 同一份签名文件的多个托管地址，按顺序尝试。都只是公开静态托管，不需要服务端。
/// 域名里不要带上游站点的关键词（实测 `huangguoai.pages.dev` 会被 DNS 污染）。
const SOURCES: [&str; 4] = [
    "https://cdn.jsdelivr.net/gh/Yishiba/HGhub@remote-config/hghub-config.json",
    "https://fastly.jsdelivr.net/gh/Yishiba/HGhub@remote-config/hghub-config.json",
    "https://testingcf.jsdelivr.net/gh/Yishiba/HGhub@remote-config/hghub-config.json",
    "https://raw.githubusercontent.com/Yishiba/HGhub/remote-config/hghub-config.json",
];

/// 本地缓存文件（应用数据目录下），保存的是原始信封，每次读取都会重新验签。
const CACHE_NAME: &str = "remote_config.json";
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
const MAX_LINES: usize = 10;
const MAX_PUBLISH_PAGES: usize = 5;
const MAX_NAME_CHARS: usize = 24;

/// 内置默认值：没有任何有效远程配置时使用，与引入远程配置之前完全一致。
pub const BUILTIN_LINES: [(&str, &str); 2] = [
    ("固定线路 1", "https://huangguoai.com"),
    ("固定线路 2", "https://huangguoai.ai"),
];
pub const BUILTIN_DEFAULT: &str = "https://huangguoai.com";
pub const BUILTIN_PUBLISH_PAGES: [&str; 1] = ["https://huangguoai.pages.dev/"];

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub version: u64,
    pub lines: Vec<Line>,
    pub default: String,
    pub publish_pages: Vec<String>,
}

#[derive(Deserialize)]
struct Envelope {
    alg: String,
    payload: String,
    sig: String,
}

#[derive(Deserialize)]
struct RawLine {
    name: String,
    url: String,
}

#[derive(Deserialize)]
struct RawConfig {
    schema: u32,
    version: u64,
    lines: Vec<RawLine>,
    default: String,
    #[serde(default)]
    publish_pages: Vec<String>,
}

static ACTIVE: Lazy<RwLock<Option<Config>>> = Lazy::new(|| RwLock::new(None));
static CACHE_FILE: Lazy<RwLock<Option<PathBuf>>> = Lazy::new(|| RwLock::new(None));

/// 验签 + 校验内容。任何一步不通过都整体拒绝，不做部分采用。
pub fn verify_envelope(text: &str, key: &[u8; 32]) -> Result<Config, String> {
    if text.len() > MAX_ENVELOPE_BYTES {
        return Err("远程配置过大".into());
    }
    let env: Envelope = serde_json::from_str(text).map_err(|_| "远程配置格式无效")?;
    if env.alg != "ed25519" {
        return Err("远程配置签名算法不受支持".into());
    }
    let payload = B64.decode(env.payload.trim()).map_err(|_| "远程配置内容编码无效")?;
    let sig_bytes: [u8; 64] = B64
        .decode(env.sig.trim())
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or("远程配置签名编码无效")?;
    let key = VerifyingKey::from_bytes(key).map_err(|_| "远程配置公钥无效")?;
    key.verify_strict(&payload, &Signature::from_bytes(&sig_bytes))
        .map_err(|_| "远程配置签名校验失败")?;
    let raw: RawConfig = serde_json::from_slice(&payload).map_err(|_| "远程配置内容无效")?;
    validate(raw)
}

fn validate(raw: RawConfig) -> Result<Config, String> {
    if raw.schema != 1 {
        return Err("远程配置版本格式不受支持（需要更新 App）".into());
    }
    if raw.version == 0 {
        return Err("远程配置版本号无效".into());
    }
    if raw.lines.is_empty() || raw.lines.len() > MAX_LINES {
        return Err("远程配置线路数量无效".into());
    }
    let mut lines: Vec<Line> = Vec::new();
    for line in raw.lines {
        let name = line.name.trim().to_string();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
            return Err("远程配置线路名称无效".into());
        }
        let url = crate::net::normalize_base(&line.url).ok_or("远程配置线路地址无效")?;
        if lines.iter().any(|l| l.url == url) {
            return Err("远程配置线路地址重复".into());
        }
        lines.push(Line { name, url });
    }
    let default = crate::net::normalize_base(&raw.default).ok_or("远程配置默认线路无效")?;
    if !lines.iter().any(|l| l.url == default) {
        return Err("远程配置默认线路不在线路清单里".into());
    }
    if raw.publish_pages.len() > MAX_PUBLISH_PAGES {
        return Err("远程配置发布页数量无效".into());
    }
    let mut publish_pages = Vec::new();
    for page in raw.publish_pages {
        let url = crate::network_policy::parse_target_with_mode(page.trim(), true)
            .map_err(|_| "远程配置发布页地址无效")?;
        if url.scheme() != "https" || url.query().is_some() || url.fragment().is_some() {
            return Err("远程配置发布页必须是 HTTPS 页面地址".into());
        }
        let url = url.to_string();
        if !publish_pages.contains(&url) {
            publish_pages.push(url);
        }
    }
    Ok(Config {
        version: raw.version,
        lines,
        default,
        publish_pages,
    })
}

/// 只有严格更新的版本才采用，防止旧配置（例如被缓存或被回放的旧文件）覆盖新配置。
pub fn should_apply(current: Option<u64>, incoming: u64) -> bool {
    current.is_none_or(|v| incoming > v)
}

fn active() -> Option<Config> {
    ACTIVE.read().ok().and_then(|c| c.clone())
}

fn active_version() -> Option<u64> {
    active().map(|c| c.version)
}

/// 启动时调用：绑定缓存位置并读回上次验签通过的配置（每次都重新验签）。
pub fn bind(dir: &Path) {
    let path = dir.join(CACHE_NAME);
    let loaded = std::fs::read_to_string(&path)
        .ok()
        .map(|text| verify_envelope(&text, &PUBLIC_KEY));
    match loaded {
        Some(Ok(cfg)) => {
            crate::playback_log::record("remote_config.cached", json!({"version": cfg.version}));
            if let Ok(mut a) = ACTIVE.write() {
                *a = Some(cfg);
            }
        }
        Some(Err(e)) => {
            // 缓存损坏或公钥已更换：丢弃，回退内置默认值，等后台拉取新配置。
            crate::playback_log::record("remote_config.cache_rejected", json!({"error": e}));
            let _ = std::fs::remove_file(&path);
        }
        None => {}
    }
    if let Ok(mut f) = CACHE_FILE.write() {
        *f = Some(path);
    }
}

/// 后台拉取：按顺序尝试各托管源，拿到**比当前更新**的版本就采用并停止。
/// 某个源验签通过但版本不比当前新（例如 jsDelivr 还缓存着旧版）时继续试下一个源，
/// 否则 CDN 的旧缓存会挡住其它源上已经发布的新版本。
pub async fn refresh() {
    let current = active_version();
    let picked = pick_newer(&SOURCES, current, |s| async move { fetch_one(s).await }).await;
    match picked {
        Pick::Newer { index, text, cfg } => {
            persist(&text);
            crate::playback_log::record(
                "remote_config.applied",
                json!({"version": cfg.version, "previous": current, "source": index}),
            );
            if let Ok(mut a) = ACTIVE.write() {
                *a = Some(cfg);
            }
        }
        Pick::NotNewer { failures } => crate::playback_log::record(
            "remote_config.not_newer",
            json!({"current": current, "failures": failures}),
        ),
        Pick::Unavailable { failures } => {
            crate::playback_log::record("remote_config.unavailable", json!({"failures": failures}))
        }
    }
}

#[derive(Debug)]
pub enum Pick {
    /// 第 `index` 个源给出了更新的版本
    Newer { index: usize, text: String, cfg: Config },
    /// 至少一个源可用，但都不比当前新
    NotNewer { failures: usize },
    /// 所有源都失败
    Unavailable { failures: usize },
}

/// 依次尝试各源，遇到更新的版本立即返回（与网络解耦，便于测试）。
pub async fn pick_newer<F, Fut>(sources: &[&'static str], current: Option<u64>, fetch: F) -> Pick
where
    F: Fn(&'static str) -> Fut,
    Fut: std::future::Future<Output = Result<(String, Config), String>>,
{
    let mut failures = 0usize;
    for (index, source) in sources.iter().enumerate() {
        match fetch(source).await {
            Ok((text, cfg)) if should_apply(current, cfg.version) => {
                return Pick::Newer { index, text, cfg };
            }
            Ok(_) => {}
            Err(e) => {
                failures += 1;
                crate::playback_log::record(
                    "remote_config.source_failed",
                    json!({"source": index, "error_kind": crate::playback_log::error_kind(&e)}),
                );
            }
        }
    }
    if failures == sources.len() {
        Pick::Unavailable { failures }
    } else {
        Pick::NotNewer { failures }
    }
}

async fn fetch_one(source: &str) -> Result<(String, Config), String> {
    let resp = crate::net::get(source, FETCH_TIMEOUT, Some("application/json"), None).await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    if resp.content_length().is_some_and(|n| n as usize > MAX_ENVELOPE_BYTES) {
        return Err("远程配置过大".into());
    }
    let text = resp.text().await.map_err(|e| crate::net::err_text(&e))?;
    let cfg = verify_envelope(&text, &PUBLIC_KEY)?;
    Ok((text, cfg))
}

fn persist(text: &str) {
    let Some(path) = CACHE_FILE.read().ok().and_then(|p| p.clone()) else {
        return;
    };
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, text)
        .and_then(|_| std::fs::rename(&temp, &path))
        .is_err()
    {
        crate::playback_log::record("remote_config.persist_failed", json!({}));
    }
}

/// 固定线路清单（名称, 地址）。
pub fn fixed_lines() -> Vec<(String, String)> {
    match active() {
        Some(cfg) => cfg.lines.into_iter().map(|l| (l.name, l.url)).collect(),
        None => BUILTIN_LINES
            .iter()
            .map(|(n, u)| (n.to_string(), u.to_string()))
            .collect(),
    }
}

/// 默认线路（用户没手动选过线路时使用）。
pub fn default_base() -> String {
    active()
        .map(|c| c.default)
        .unwrap_or_else(|| BUILTIN_DEFAULT.to_string())
}

/// 镜像发布页清单，按顺序尝试。配置里给了空清单就表示不提供镜像线路。
pub fn publish_pages() -> Vec<String> {
    match active() {
        Some(cfg) => cfg.publish_pages,
        None => BUILTIN_PUBLISH_PAGES.iter().map(|s| s.to_string()).collect(),
    }
}

/// 诊断 / 线路页展示用：当前生效的配置版本，`None` 表示使用内置默认值。
pub fn status() -> serde_json::Value {
    json!({ "version": active_version(), "builtin": active().is_none() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试专用公钥，对应的私钥只用来签 `fixtures/remote-config-test.json`，与正式密钥无关。
    /// 这份样例由 `scripts/remote-config.mjs sign` 生成，用来保证 Node 签名与 Rust 验签格式一致。
    const TEST_KEY_B64: &str = "oP5UgWuToJ513Vs9r2MOOeHA+6ncgYGkmaR3KHDA2qg=";
    const FIXTURE: &str = include_str!("../fixtures/remote-config-test.json");

    fn test_key() -> [u8; 32] {
        B64.decode(TEST_KEY_B64).unwrap().try_into().unwrap()
    }

    fn signed(payload: &serde_json::Value) -> String {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let bytes = serde_json::to_vec(payload).unwrap();
        let sig = sk.sign(&bytes);
        json!({"alg": "ed25519", "payload": B64.encode(&bytes), "sig": B64.encode(sig.to_bytes())})
            .to_string()
    }

    fn inline_key() -> [u8; 32] {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes()
    }

    fn sample() -> serde_json::Value {
        json!({
            "schema": 1, "version": 3,
            "lines": [{"name": "线路 1", "url": "https://one.example.com"},
                      {"name": "线路 2", "url": "https://two.example.com/"}],
            "default": "https://two.example.com",
            "publish_pages": ["https://pub.example.org/"]
        })
    }

    #[test]
    fn node_signed_fixture_verifies() {
        let cfg = verify_envelope(FIXTURE, &test_key()).expect("Node 签名的样例应能通过 Rust 验签");
        assert_eq!(cfg.version, 7);
        assert_eq!(cfg.default, "https://b.example.com");
        assert_eq!(cfg.lines.len(), 2);
        assert_eq!(cfg.publish_pages, vec!["https://pub.example.com/".to_string()]);
    }

    #[test]
    fn fixture_is_rejected_by_the_production_key() {
        assert!(verify_envelope(FIXTURE, &PUBLIC_KEY).is_err());
    }

    #[test]
    fn tampered_payload_or_signature_is_rejected() {
        let mut env: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let mut payload: serde_json::Value =
            serde_json::from_slice(&B64.decode(env["payload"].as_str().unwrap()).unwrap()).unwrap();
        payload["default"] = json!("https://a.example.com");
        env["payload"] = json!(B64.encode(serde_json::to_vec(&payload).unwrap()));
        assert!(verify_envelope(&env.to_string(), &test_key()).is_err());

        let mut env: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        env["sig"] = json!(B64.encode([0u8; 64]));
        assert!(verify_envelope(&env.to_string(), &test_key()).is_err());

        let mut env: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        env["alg"] = json!("none");
        assert!(verify_envelope(&env.to_string(), &test_key()).is_err());
        assert!(verify_envelope("not json", &test_key()).is_err());
        assert!(verify_envelope(&" ".repeat(MAX_ENVELOPE_BYTES + 1), &test_key()).is_err());
    }

    #[test]
    fn valid_config_is_normalized() {
        let cfg = verify_envelope(&signed(&sample()), &inline_key()).unwrap();
        assert_eq!(cfg.lines[1].url, "https://two.example.com");
        assert_eq!(cfg.default, "https://two.example.com");
    }

    #[test]
    fn unsafe_or_inconsistent_content_is_rejected_even_when_signed() {
        let cases: Vec<(&str, serde_json::Value)> = vec![
            ("schema", json!(2)),
            ("version", json!(0)),
            ("lines", json!([])),
            ("default", json!("https://three.example.com")),
            ("publish_pages", json!(["http://pub.example.org/"])),
            ("publish_pages", json!(["https://192.168.1.1/"])),
            ("publish_pages", json!(["https://a/", "https://b/", "https://c/", "https://d/", "https://e/", "https://f/"])),
        ];
        for (field, value) in cases {
            let mut payload = sample();
            payload[field] = value.clone();
            assert!(
                verify_envelope(&signed(&payload), &inline_key()).is_err(),
                "{field} = {value} 应被拒绝"
            );
        }
        // 非法线路排在合法线路之后，默认线路始终指向合法的那条：
        // 这样被拒只能是线路校验本身起作用，而不是被默认线路校验顺带拒掉。
        let bad_lines = [
            json!({"name": "x", "url": "http://bad.example.com"}),
            json!({"name": "x", "url": "https://bad.example.com/path"}),
            json!({"name": "x", "url": "https://127.0.0.1"}),
            json!({"name": "x", "url": "https://user:pw@bad.example.com"}),
            json!({"name": "", "url": "https://bad.example.com"}),
            json!({"name": "x".repeat(MAX_NAME_CHARS + 1), "url": "https://bad.example.com"}),
            json!({"name": "dup", "url": "https://one.example.com/"}),
        ];
        for bad in bad_lines {
            let mut payload = sample();
            payload["lines"] = json!([{"name": "线路 1", "url": "https://one.example.com"}, bad.clone()]);
            payload["default"] = json!("https://one.example.com");
            assert!(
                verify_envelope(&signed(&payload), &inline_key()).is_err(),
                "线路 {bad} 应被拒绝"
            );
        }
        let mut too_many = sample();
        too_many["lines"] = json!((0..=MAX_LINES)
            .map(|i| json!({"name": format!("线路 {i}"), "url": format!("https://l{i}.example.com")}))
            .collect::<Vec<_>>());
        too_many["default"] = json!("https://l0.example.com");
        assert!(verify_envelope(&signed(&too_many), &inline_key()).is_err());
    }

    #[test]
    fn only_strictly_newer_versions_are_applied() {
        assert!(should_apply(None, 1));
        assert!(should_apply(Some(3), 4));
        assert!(!should_apply(Some(3), 3));
        assert!(!should_apply(Some(3), 2));
    }

    #[tokio::test]
    #[ignore = "联网测试：从各托管源拉取正式配置并用正式公钥验签，手动用 --ignored 运行"]
    async fn sources_live() {
        let mut ok = 0;
        for (i, source) in SOURCES.iter().enumerate() {
            match fetch_one(source).await {
                Ok((_, cfg)) => {
                    ok += 1;
                    println!("源 {i} 正常：version={} 默认={} 发布页={:?}", cfg.version, cfg.default, cfg.publish_pages);
                }
                Err(e) => println!("源 {i} 失败：{e}"),
            }
        }
        assert!(ok > 0, "所有托管源都拿不到有效配置");
    }

    fn cfg(version: u64) -> Config {
        Config { version, lines: vec![], default: String::new(), publish_pages: vec![] }
    }

    #[tokio::test]
    async fn stale_cdn_does_not_shadow_newer_source() {
        // 前三个源是 CDN 旧缓存 v1，最后一个源已经是 v2
        let sources: [&'static str; 4] = ["cdn", "fastly", "testingcf", "raw"];
        let pick = pick_newer(&sources, Some(1), |s| async move {
            Ok((s.to_string(), cfg(if s == "raw" { 2 } else { 1 })))
        })
        .await;
        match pick {
            Pick::Newer { index, cfg, .. } => {
                assert_eq!(index, 3);
                assert_eq!(cfg.version, 2);
            }
            other => panic!("应采用 raw 上的 v2，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn first_newer_source_wins_and_failures_are_counted() {
        let sources: [&'static str; 3] = ["a", "b", "c"];
        let pick = pick_newer(&sources, None, |s| async move {
            if s == "a" { Err("超时".to_string()) } else { Ok((s.to_string(), cfg(5))) }
        })
        .await;
        assert!(matches!(pick, Pick::Newer { index: 1, .. }), "{pick:?}");
        let pick = pick_newer(&sources, Some(5), |s| async move { Ok((s.to_string(), cfg(5))) }).await;
        assert!(matches!(pick, Pick::NotNewer { failures: 0 }), "{pick:?}");
        let pick = pick_newer(&sources, Some(5), |_| async move { Err::<(String, Config), _>("x".to_string()) }).await;
        assert!(matches!(pick, Pick::Unavailable { failures: 3 }), "{pick:?}");
    }

    #[test]
    fn builtin_defaults_match_previous_behavior() {
        assert_eq!(BUILTIN_DEFAULT, BUILTIN_LINES[0].1);
        assert!(crate::net::trusted_auth_origin(BUILTIN_DEFAULT));
        for (_, url) in BUILTIN_LINES {
            assert_eq!(crate::net::normalize_base(url).as_deref(), Some(url));
        }
    }
}
