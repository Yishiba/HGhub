//! 冷启动自动选线路：首选线路连不上时，自动换到一条真正能用的线路。
//!
//! - **首选线路**是用户手动选过的线路（`line.json`），没选过就是默认线路。能用就始终用它。
//! - 首选线路连不上时：先试上次自动选中的线路（`line_auto.json`，大多数启动到这一步就结束），
//!   再从固定线路 + 发布页上的最新镜像（每个域名抽 2 个，最多 12 个）里并发检测，**最先成功的**胜出。
//! - 自动选中的线路只改当前线路、记到 `line_auto.json`，**不覆盖用户的手动选择、不清登录态**
//!   （会话与站点绑定：镜像上自然是游客，首选线路恢复后登录态还在）。首选线路恢复后清掉自动记录，
//!   下次启动回到首选线路。
//! - 检测走真实接口（`/api/videos?page=1&size=1` 返回业务成功才算），不是只看首页能不能打开：
//!   `huangguoai.ai` 首页 200 但接口 404，只看首页会被误判为可用。
//! - 业务请求在 [`wait_ready`] 等选线路结束（有上限），不会抢先打到被墙的线路上；
//!   检测本身直接调 `net::edge`，不经过这道闸，不会自己等自己。

use once_cell::sync::Lazy;
use serde_json::json;
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const PROBE_PATH: &str = "/api/videos?page=1&size=1";
/// 单条线路的检测上限
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
/// 业务请求最多等选线路这么久，超时就按当前线路照常请求
const READY_WAIT: Duration = Duration::from_secs(25);
/// 上次自动线路已经可用时，再给首选线路这么久：首选线路恢复了就直接回到首选（登录态可用）
const PREFERRED_GRACE: Duration = Duration::from_millis(1500);
const MIRRORS_PER_DOMAIN: usize = 2;
const MAX_SAMPLED_MIRRORS: usize = 12;

static READY: Lazy<watch::Sender<bool>> = Lazy::new(|| watch::channel(false).0);

/// 等冷启动选线路结束（最多 [`READY_WAIT`]）。选线路结束后立即返回。
pub async fn wait_ready() {
    let mut rx = READY.subscribe();
    let _ = tokio::time::timeout(READY_WAIT, rx.wait_for(|ready| *ready)).await;
}

/// 用真实接口检测一条线路：业务成功才算可用。返回耗时（毫秒）。
pub async fn api_probe(origin: &str) -> Result<u64, String> {
    let started = Instant::now();
    match tokio::time::timeout(
        PROBE_TIMEOUT,
        crate::net::edge(origin, "GET", PROBE_PATH, None, None),
    )
    .await
    {
        Ok(Ok(_)) => Ok(started.elapsed().as_millis() as u64),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("检测超时".into()),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// 用首选线路（能用，或者谁都不能用时也保持首选）
    Preferred,
    /// 换到这条线路
    Auto(String),
    /// 首选线路和所有候选都连不上：保持首选线路，界面走「切换线路」引导
    NoneReachable,
}

pub struct Outcome {
    pub decision: Decision,
    /// 选了上次的自动线路但首选线路还没测完：后台继续等它，成功了就清掉自动记录
    pub pending_preferred: Option<JoinHandle<bool>>,
}

/// 选线路的决策，与真实网络解耦（`probe` / `candidates` 由调用方注入），便于测试。
pub async fn decide<P, Fut, C, CFut>(
    preferred: &str,
    last_auto: Option<&str>,
    probe: P,
    candidates: C,
    grace: Duration,
) -> Outcome
where
    P: Fn(String) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = bool> + Send + 'static,
    C: FnOnce() -> CFut,
    CFut: Future<Output = Vec<String>>,
{
    let last_auto = last_auto.filter(|l| *l != preferred);
    let mut tried: Vec<String> = vec![preferred.to_string()];

    match last_auto {
        Some(last) => {
            tried.push(last.to_string());
            let mut pref_task = tokio::spawn((probe.clone())(preferred.to_string()));
            if (probe.clone())(last.to_string()).await {
                // 上次的自动线路能用。首选线路如果很快也通了，就回到首选线路。
                return match tokio::time::timeout(grace, &mut pref_task).await {
                    Ok(Ok(true)) => Outcome { decision: Decision::Preferred, pending_preferred: None },
                    Ok(_) => Outcome { decision: Decision::Auto(last.to_string()), pending_preferred: None },
                    Err(_) => Outcome {
                        decision: Decision::Auto(last.to_string()),
                        pending_preferred: Some(pref_task),
                    },
                };
            }
            if pref_task.await.unwrap_or(false) {
                return Outcome { decision: Decision::Preferred, pending_preferred: None };
            }
        }
        None => {
            if (probe.clone())(preferred.to_string()).await {
                return Outcome { decision: Decision::Preferred, pending_preferred: None };
            }
        }
    }

    // 首选线路和上次的自动线路都不行：并发检测其余候选，最先成功的胜出
    let mut set = tokio::task::JoinSet::new();
    let mut seen = tried;
    for url in candidates().await {
        if seen.contains(&url) {
            continue;
        }
        seen.push(url.clone());
        let p = probe.clone();
        set.spawn(async move { (p(url.clone()).await, url) });
    }
    while let Some(joined) = set.join_next().await {
        if let Ok((true, url)) = joined {
            set.abort_all();
            return Outcome { decision: Decision::Auto(url), pending_preferred: None };
        }
    }
    Outcome { decision: Decision::NoneReachable, pending_preferred: None }
}

/// 镜像是「子域 × 域名」的笛卡尔积，同一域名的子域通常同生共死：每个域名只抽前几个。
pub fn sample_mirrors(urls: &[String]) -> Vec<String> {
    let mut per_domain: Vec<(String, usize)> = Vec::new();
    let mut out = Vec::new();
    for url in urls {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        let domain = host.split_once('.').map(|(_, d)| d.to_string()).unwrap_or(host);
        let count = match per_domain.iter_mut().find(|(d, _)| *d == domain) {
            Some((_, n)) => n,
            None => {
                per_domain.push((domain, 0));
                &mut per_domain.last_mut().unwrap().1
            }
        };
        if *count >= MIRRORS_PER_DOMAIN {
            continue;
        }
        *count += 1;
        out.push(url.clone());
        if out.len() >= MAX_SAMPLED_MIRRORS {
            break;
        }
    }
    out
}

/// 候选线路：固定线路 + 发布页上的最新镜像抽样（发布页拿不到就只有固定线路）。
async fn candidates() -> Vec<String> {
    let mut out: Vec<String> = crate::remote_config::fixed_lines()
        .into_iter()
        .map(|(_, url)| url)
        .collect();
    if let Ok(v) = crate::lines::mirrors().await {
        let urls: Vec<String> = v["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|it| it["url"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        out.extend(sample_mirrors(&urls));
    }
    out
}

/// 冷启动时在后台运行一次。结束后放行业务请求。
pub async fn run() {
    let started = Instant::now();
    let epoch = crate::net::manual_epoch();
    let preferred = crate::net::preferred_base();
    let last = crate::net::last_auto_base();
    let outcome = decide(
        &preferred,
        last.as_deref(),
        |url: String| async move { api_probe(&url).await.is_ok() },
        candidates,
        PREFERRED_GRACE,
    )
    .await;

    // 选线路期间用户手动换过线路：以用户为准，丢弃自动结果
    let manual = crate::net::manual_epoch() != epoch;
    if !manual {
        match &outcome.decision {
            Decision::Preferred => {
                if last.is_some() {
                    crate::net::clear_auto_base();
                }
            }
            Decision::Auto(url) => crate::net::apply_auto_base(url),
            Decision::NoneReachable => {}
        }
    }
    crate::playback_log::record(
        "line.select_done",
        json!({
            "decision": match &outcome.decision {
                Decision::Preferred => "preferred",
                Decision::Auto(_) => "auto",
                Decision::NoneReachable => "none",
            },
            "had_last_auto": last.is_some(),
            "manual_override": manual,
            "ms": started.elapsed().as_millis() as u64,
        }),
    );
    READY.send_replace(true);

    if let Some(task) = outcome.pending_preferred {
        if task.await.unwrap_or(false) {
            crate::net::clear_auto_base();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// 假探测：url → (是否可用, 延迟毫秒)；未登记的一律不可用。同时记录探测过哪些地址。
    fn fake(table: &[(&str, bool, u64)]) -> (impl Fn(String) -> std::pin::Pin<Box<dyn Future<Output = bool> + Send>> + Clone + Send + 'static, Arc<Mutex<Vec<String>>>) {
        let table: Vec<(String, bool, u64)> = table.iter().map(|(u, ok, ms)| (u.to_string(), *ok, *ms)).collect();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let probe = move |url: String| {
            let entry = table.iter().find(|(u, _, _)| *u == url).cloned();
            log.lock().unwrap().push(url);
            Box::pin(async move {
                match entry {
                    Some((_, ok, ms)) => {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        ok
                    }
                    None => false,
                }
            }) as std::pin::Pin<Box<dyn Future<Output = bool> + Send>>
        };
        (probe, seen)
    }

    fn list(urls: &[&str]) -> Vec<String> {
        urls.iter().map(|s| s.to_string()).collect()
    }

    const GRACE: Duration = Duration::from_millis(200);

    #[tokio::test]
    async fn preferred_reachable_is_kept_and_no_candidates_are_probed() {
        let (probe, seen) = fake(&[("https://pref", true, 5), ("https://m1", true, 1)]);
        let out = decide("https://pref", None, probe, || async { list(&["https://m1"]) }, GRACE).await;
        assert_eq!(out.decision, Decision::Preferred);
        assert_eq!(*seen.lock().unwrap(), vec!["https://pref".to_string()]);
    }

    #[tokio::test]
    async fn last_auto_is_used_when_preferred_is_down() {
        let (probe, _) = fake(&[("https://pref", false, 30), ("https://last", true, 5)]);
        let out = decide("https://pref", Some("https://last"), probe, || async { Vec::new() }, GRACE).await;
        assert_eq!(out.decision, Decision::Auto("https://last".into()));
    }

    #[tokio::test]
    async fn preferred_wins_back_when_it_recovers_quickly() {
        let (probe, _) = fake(&[("https://pref", true, 20), ("https://last", true, 5)]);
        let out = decide("https://pref", Some("https://last"), probe, || async { Vec::new() }, GRACE).await;
        assert_eq!(out.decision, Decision::Preferred);
    }

    #[tokio::test]
    async fn slow_preferred_keeps_last_auto_and_reports_recovery_later() {
        let (probe, _) = fake(&[("https://pref", true, 600), ("https://last", true, 5)]);
        let out = decide("https://pref", Some("https://last"), probe, || async { Vec::new() }, GRACE).await;
        assert_eq!(out.decision, Decision::Auto("https://last".into()));
        let pending = out.pending_preferred.expect("首选线路还在检测，应交给后台");
        assert!(pending.await.unwrap(), "首选线路最终可用，应在后台报告");
    }

    #[tokio::test]
    async fn fastest_reachable_candidate_wins() {
        let (probe, seen) = fake(&[
            ("https://pref", false, 5),
            ("https://dead", false, 5),
            ("https://slow", true, 300),
            ("https://fast", true, 20),
        ]);
        let out = decide("https://pref", None, probe, || async {
            list(&["https://pref", "https://dead", "https://slow", "https://fast"])
        }, GRACE).await;
        assert_eq!(out.decision, Decision::Auto("https://fast".into()));
        let probed = seen.lock().unwrap().clone();
        assert_eq!(probed.iter().filter(|u| *u == "https://pref").count(), 1, "首选线路不重复检测");
    }

    #[tokio::test]
    async fn nothing_reachable_keeps_preferred() {
        let (probe, _) = fake(&[("https://pref", false, 5), ("https://last", false, 5)]);
        let out = decide("https://pref", Some("https://last"), probe, || async { list(&["https://m1"]) }, GRACE).await;
        assert_eq!(out.decision, Decision::NoneReachable);
    }

    #[tokio::test]
    async fn last_auto_equal_to_preferred_is_ignored() {
        let (probe, seen) = fake(&[("https://pref", false, 5), ("https://m1", true, 5)]);
        let out = decide("https://pref", Some("https://pref"), probe, || async { list(&["https://m1"]) }, GRACE).await;
        assert_eq!(out.decision, Decision::Auto("https://m1".into()));
        assert_eq!(seen.lock().unwrap().iter().filter(|u| *u == "https://pref").count(), 1);
    }

    #[tokio::test]
    #[ignore = "联网测试：用真实接口检测几条线路，手动用 --ignored 运行"]
    async fn api_probe_live() {
        for url in ["https://huangguoai.com", "https://huangguoai.ai", "https://thu.dvygahpw.cc"] {
            match api_probe(url).await {
                Ok(ms) => println!("{url} 可用 {ms}ms"),
                Err(e) => println!("{url} 不可用：{}", e.chars().take(80).collect::<String>()),
            }
        }
    }

    #[test]
    fn mirrors_are_sampled_per_domain() {
        let urls = list(&[
            "https://thu.a.cc", "https://pku.a.cc", "https://fdu.a.cc",
            "https://thu.b.cc", "https://pku.b.cc", "https://fdu.b.cc",
        ]);
        assert_eq!(
            sample_mirrors(&urls),
            list(&["https://thu.a.cc", "https://pku.a.cc", "https://thu.b.cc", "https://pku.b.cc"])
        );
        let many: Vec<String> = (0..20).map(|i| format!("https://x.d{i}.cc")).collect();
        assert_eq!(sample_mirrors(&many).len(), MAX_SAMPLED_MIRRORS);
    }
}
