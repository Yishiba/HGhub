import { useEffect, useState } from "react";
import { api, hostOf, isMobile, type LineItem, type LineProbe, type LineState } from "../api";
import { useStore } from "../store";
import { Modal, Sheet } from "./ui";
import { ICheck, IClose, IRefresh, ISettings } from "../icons";

type ProbeMap = Record<string, LineProbe>;

/** 检测过且能连上的排最前（按耗时），连不上的其次，没检测过的保持原顺序 */
function sortByProbe(list: LineItem[], probes: ProbeMap): LineItem[] {
  const rank = (it: LineItem) => {
    const p = probes[it.url];
    return p ? (p.ok ? 0 : 2) : 1;
  };
  return list
    .map((it, i) => ({ it, i }))
    .sort((a, b) => rank(a.it) - rank(b.it)
      || (rank(a.it) === 0 ? (probes[a.it.url].ms - probes[b.it.url].ms) : 0)
      || a.i - b.i)
    .map((x) => x.it);
}

/** 镜像是「子域 × 域名」的笛卡尔积，同一域名的子域通常同生共死：
 *  引导检测时每个域名只抽前 2 个，总数不超过 12，避免一次探测几十条。 */
function sampleMirrors(list: LineItem[]): LineItem[] {
  const perDomain: Record<string, number> = {};
  const out: LineItem[] = [];
  for (const it of list) {
    const host = it.host || hostOf(it.url);
    const domain = host.split(".").slice(1).join(".") || host;
    if ((perDomain[domain] || 0) >= 2) continue;
    perDomain[domain] = (perDomain[domain] || 0) + 1;
    out.push(it);
    if (out.length >= 12) break;
  }
  return out;
}

/* 单条线路：左侧点一下即切换，右侧单独检测 */
function Row({ item, current, probe, busy, disabled, onPick, onProbe }: {
  item: LineItem; current: boolean; probe?: LineProbe; busy: boolean; disabled: boolean;
  onPick: (url: string) => void; onProbe: (url: string) => void;
}) {
  return (
    <div className={`flex items-center gap-2 px-3.5 py-2.5 ${current ? "bg-s3/70" : ""}`}>
      <button onClick={() => onPick(item.url)} disabled={disabled} className="flex-1 min-w-0 text-left pressable disabled:opacity-60">
        <div className="flex items-center gap-1.5 text-[12.5px] text-on">
          <span className="truncate">{item.name || item.host || hostOf(item.url)}</span>
          {current && <ICheck size={13} className="text-mint shrink-0" />}
        </div>
        <div className="text-[10.5px] text-mut truncate mt-0.5">{item.url}</div>
      </button>
      <span
        title={probe ? (probe.error || (probe.status ? `HTTP ${probe.status}` : "")) : ""}
        className={`text-[10.5px] tabular-nums shrink-0 ${probe ? (probe.ok ? "text-mint" : "text-danger") : "text-dim"}`}>
        {busy ? "…" : probe ? (probe.ok ? `${probe.ms}ms` : probe.status ? `HTTP ${probe.status}` : "失败") : "—"}
      </span>
      <button onClick={() => onProbe(item.url)} disabled={busy || disabled}
        className="shrink-0 h-7 px-2.5 rounded-(--r-sm) bg-s4 text-[11px] text-on/80 pressable disabled:opacity-50">检测</button>
    </div>
  );
}

/* 线路选择：桌面居中弹窗，移动端底部抽屉 */
export default function LinePicker({ open, guided = false, onClose }: {
  open: boolean;
  /** 从「当前线路连不上」的报错页打开：自动刷新镜像并检测，选好后关闭面板 */
  guided?: boolean;
  onClose: () => void;
}) {
  const { toast, setUser, bumpLineEpoch } = useStore();
  const [state, setState] = useState<LineState | null>(null);
  const [mirrors, setMirrors] = useState<LineItem[] | null>(null);
  const [mirrorErr, setMirrorErr] = useState("");
  const [loadingMirrors, setLoadingMirrors] = useState(false);
  const [probes, setProbes] = useState<ProbeMap>({});
  const [probing, setProbing] = useState("");
  const [switching, setSwitching] = useState("");

  const readState = () => {
    api.lineState().then(setState).catch((e) => toast((e as Error).message || "读取线路失败", "info"));
  };

  const readMirrors = async (): Promise<LineItem[]> => {
    setLoadingMirrors(true);
    setMirrorErr("");
    try {
      const res = await api.lineMirrors();
      const items = res?.items || [];
      setMirrors(items);
      return items;
    } catch (e) {
      setMirrors([]);
      setMirrorErr((e as Error).message || "镜像线路获取失败");
      return [];
    } finally {
      setLoadingMirrors(false);
    }
  };

  /* 平时：镜像线路只在用户点开「线路」后才去拉，拉过一次就留在内存里，可手动刷新。
     引导模式：镜像域名会轮换（旧域名被墙后发布页换新），所以每次都重新拉，并自动检测一遍。 */
  useEffect(() => {
    if (!open) return;
    if (!guided) {
      readState();
      if (mirrors === null) void readMirrors();
      return;
    }
    setProbes({});
    void (async () => {
      const [st, list] = await Promise.all([
        api.lineState().catch(() => null),
        readMirrors(),
      ]);
      if (st) setState(st);
      await probe([...(st?.fixed || []), ...sampleMirrors(list)], "guided");
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, guided]);

  const probe = async (list: LineItem[], key: string) => {
    if (!list.length) return;
    setProbing(key);
    try {
      const res = await api.lineProbe(list.map((x) => x.url));
      setProbes((p) => {
        const next = { ...p };
        for (const r of res) next[r.url] = r;
        return next;
      });
    } catch (e) {
      toast((e as Error).message || "检测失败", "info");
    } finally {
      setProbing("");
    }
  };

  const pick = async (url: string) => {
    if (switching || state?.base === url) return;
    setSwitching(url);
    try {
      const next = await api.lineSet(url);
      if (next !== state?.base) setUser(null);
      setState((s) => (s ? { ...s, base: next } : s));
      bumpLineEpoch();
      toast(`线路已切换到 ${hostOf(next)}，跨站点需重新登录`, "ok");
      if (guided) onClose();
    } catch (e) {
      toast((e as Error).message || "切换失败", "info");
    } finally {
      setSwitching("");
    }
  };

  const fixed = sortByProbe(state?.fixed || [], probes);
  const mirrorRows = mirrors ? sortByProbe(mirrors, probes) : null;
  const okCount = Object.values(probes).filter((p) => p.ok).length;
  const busyAll = !!probing || !!switching;

  const body = (
    <div className="flex flex-col min-h-0" style={{ maxHeight: isMobile ? "74vh" : "min(72vh, 620px)" }}>
      <div className="shrink-0 flex items-center gap-2 px-4 pt-3 pb-2">
        <span className="w-8 h-8 rounded-full bg-s3 flex items-center justify-center text-primary"><ISettings size={16} /></span>
        <div className="flex-1 min-w-0">
          <div className="text-[14px] font-bold text-on leading-tight">线路</div>
          <div className="text-[10.5px] text-mut mt-0.5 truncate">当前：{state ? hostOf(state.base) : "读取中…"}{state?.auto ? "（启动时自动选择）" : ""}</div>
        </div>
        <button onClick={onClose} aria-label="关闭" className="w-8 h-8 rounded-full bg-s3/80 flex items-center justify-center text-mut pressable"><IClose size={16} /></button>
      </div>

      <div className="flex-1 scroll-y px-3 pb-4">
        {guided && (
          <div className="mx-1 mb-2 rounded-(--r-md) bg-s2 px-3 py-2.5 text-[11px] leading-relaxed text-on/85">
            {probing === "guided"
              ? "当前线路连不上，正在拉取最新镜像并检测可用线路…"
              : okCount
                ? `当前线路连不上，多半是被网络限制。找到 ${okCount} 条能连上的线路（已排在前面），点一条即可切换。`
                : "当前线路连不上，检测到的线路也都连不上。可以稍后再试，或开启代理 / VPN 后再选择。"}
          </div>
        )}
        <div className="flex items-center gap-2 px-1 mt-1 mb-1.5">
          <span className="text-[10.5px] font-semibold text-dim tracking-wide">固定线路</span>
          <span className="text-[10px] text-dim">需代理 / 非大陆网络</span>
          <button onClick={() => probe(fixed, "fixed")} disabled={busyAll || !fixed.length}
            className="ml-auto text-[10.5px] text-primary pressable disabled:opacity-50">
            {probing === "fixed" ? "检测中…" : "全部检测"}
          </button>
        </div>
        <div className="rounded-(--r-md) bg-s2 divide-y divide-s3 overflow-hidden">
          {fixed.map((it) => (
            <Row key={it.url} item={it} current={state?.base === it.url} probe={probes[it.url]}
              busy={probing === it.url} disabled={busyAll} onPick={pick} onProbe={(u) => probe([{ url: u }], u)} />
          ))}
          {!fixed.length && <div className="px-3.5 py-3 text-[11px] text-mut">读取中…</div>}
        </div>

        <div className="flex items-center gap-2 px-1 mt-4 mb-1.5">
          <span className="text-[10.5px] font-semibold text-dim tracking-wide">镜像线路</span>
          <span className="text-[10px] text-dim">一般可直连 · 仅游客浏览</span>
          <button onClick={() => { void readMirrors(); }} disabled={loadingMirrors}
            className="ml-auto inline-flex items-center gap-1 text-[10.5px] text-primary pressable disabled:opacity-50">
            <IRefresh size={12} />{loadingMirrors ? "获取中…" : "刷新"}
          </button>
        </div>
        <div className="rounded-(--r-md) bg-s2 divide-y divide-s3 overflow-hidden">
          {loadingMirrors && <div className="flex justify-center py-6"><div className="w-6 h-6 spinner" /></div>}
          {!loadingMirrors && mirrorErr && (
            <div className="px-3.5 py-3">
              <div className="text-[11.5px] text-danger">镜像线路获取失败</div>
              <div className="text-[10.5px] text-mut mt-1 break-all">{mirrorErr}</div>
            </div>
          )}
          {!loadingMirrors && !mirrorErr && mirrors?.length === 0 && (
            <div className="px-3.5 py-3 text-[11px] text-mut">地址发布页里没有可用的镜像地址</div>
          )}
          {!loadingMirrors && (mirrorRows || []).map((it) => (
            <Row key={it.url} item={it} current={state?.base === it.url} probe={probes[it.url]}
              busy={probing === it.url} disabled={busyAll} onPick={pick} onProbe={(u) => probe([{ url: u }], u)} />
          ))}
        </div>
        {!loadingMirrors && !!mirrors?.length && (
          <div className="flex justify-end px-1 mt-2">
            <button onClick={() => probe(mirrors, "mirrors")} disabled={busyAll}
              className="text-[10.5px] text-primary pressable disabled:opacity-50">
              {probing === "mirrors" ? "检测中…" : `全部检测（${mirrors.length}）`}
            </button>
          </div>
        )}

        <div className="text-[10px] text-dim leading-relaxed px-1 mt-4">
          切换后立即生效，请求链路与数据模型不变。启动时如果你选的线路连不上，会自动换到一条能用的线路（不改你的选择，恢复后自动换回）；手动选择的线路始终优先。检测会真的请求一次接口，绿色表示能正常拉到内容。
        </div>
      </div>
    </div>
  );

  return isMobile
    ? <Sheet open={open} onClose={onClose}>{body}</Sheet>
    : <Modal open={open} onClose={onClose} width={460}>{body}</Modal>;
}
