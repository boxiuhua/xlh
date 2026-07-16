//! 抓取周期编排：交易日判断 → 快照 → 落库 → 异动 → 资金流 → 标签 → 推送。
//!
//! 遵循本项目既有的分离约定（`channels.rs:1-2`「构造与发送分离，前者纯函数
//! 便于测试」、`schedule.rs` 的 `due_users` 纯函数）：**决策逻辑是纯函数，
//! IO 在外层**。`select_pushable` / `render_movers` / `render_summary` 全都
//! 不碰网络不碰库，可直接单测。
use std::collections::HashSet;
use anyhow::{anyhow, Result};
use chrono::{Local, NaiveDate, NaiveDateTime};
use rusqlite::Connection;

use super::config::RealtimeCfg;
use super::movers::{self, Baseline, Divergence, Horizon, Mover};
use super::store::{self, SignalRow};
use super::{calendar, flow, snapshot};

/// 长线判定的估值分位门槛：PE 处于历史 30% 分位以下算「便宜」。
///
/// 与其它阈值一样**未经检验**。之所以不进 config：它是标签的定义而非灵敏度
/// 旋钮，且 `valuation::percentile` 本身要求 ≥60 个样本才给分位，
/// 已有一层保护。日后若要调，连同 classify 的整套逻辑一起重估。
const LOW_PE_PCT: f64 = 0.30;

/// 一个抓取时点的产出。
#[derive(Debug, Clone, PartialEq)]
pub struct TickOutcome {
    /// 本时点写入的快照数
    pub ticks: usize,
    /// 检出的全部异动（已排序、已打标签）。全部进库。
    pub movers: Vec<Mover>,
    /// 经三层限流后**应当推送**的那批。
    ///
    /// 这里存 Mover 而非数量：`movers` 是全部异动，`pushed` 是它的一个
    /// **过滤子集**（强信号 ∩ 今日未推过），二者顺序不对应。
    /// 若只回传数量、让调用方 `movers.take(n)`，会推错股票 ——
    /// 比如排第一的异动今天已推过、本该被过滤，却仍被 take 捞出来重推。
    pub pushed: Vec<Mover>,
    /// 资金流是否可用（东财封禁时为 false）
    pub flow_ok: bool,
}

/// 跳过的原因。区分「不该抓」与「抓了但不是交易日」——
/// 前者无声，后者要落 non_trading_days 免得当天再撞 19 次。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Skip {
    Weekend,
    NotTickTime,
    KnownHoliday,
    /// 快照自证失败：拿到的是陈旧数据 → 今天是节假日
    StaleData,
}

/// 纯函数：给定当前时刻与库状态，判断这一 tick 该不该抓。
pub fn should_run(conn: &Connection, now: NaiveDateTime) -> Result<Option<Skip>> {
    if calendar::is_weekend(now.date()) { return Ok(Some(Skip::Weekend)) }
    if !calendar::is_tick_time(now.time()) { return Ok(Some(Skip::NotTickTime)) }
    if store::is_non_trading(conn, now.date())? { return Ok(Some(Skip::KnownHoliday)) }
    Ok(None)
}

/// 三层限流后的可推送集合。**纯函数**。
///
/// 1. 同一只股票当日只推一次（`already` 来自 signals 表，重启不丢）
/// 2. 只推强信号（≥阈值×strong_signal_x），弱信号只进库
/// 3. 每时点最多 max_push_per_tick 只，超出按资金流权重排序取前 N
///
/// 没有这三层，一天 20 时点 × 数十只 = 数百条，飞书会被刷到静音 ——
/// 那等于这个功能白做。
pub fn select_pushable(movers: &[Mover], already: &HashSet<String>, cfg: &RealtimeCfg) -> Vec<Mover> {
    let strong: Vec<Mover> = movers.iter()
        .filter(|m| !already.contains(&m.code))
        .filter(|m| movers::is_strong(
            m.jump_pct, m.vol_surge_x, cfg.price_jump_pct, cfg.volume_surge_x, cfg.strong_signal_x))
        .cloned()
        .collect();
    movers::rank_top(strong, cfg.max_push_per_tick)
}

fn dir_arrow(pct: f64) -> &'static str { if pct >= 0.0 { "涨" } else { "跌" } }

fn flow_text(m: &Mover) -> String {
    match m.main_net_pct {
        Some(p) => format!("主力{}{:.1}%", if p >= 0.0 { "净流入" } else { "净流出" }, p.abs() * 100.0),
        None => "资金流暂不可用".to_string(),
    }
}

fn divergence_text(d: Divergence) -> &'static str {
    match d {
        Divergence::RetailChasing => "⚠ 涨但主力净流出（疑似散户抬轿）",
        Divergence::MainAccumulating => "⚠ 跌但主力净流入（疑似主力吸筹）",
        Divergence::None | Divergence::Unknown => "",
    }
}

fn horizon_text(h: Horizon) -> &'static str {
    match h { Horizon::Short => "短线", Horizon::Long => "长线" }
}

/// 免责声明。与 `StockRecommendReport.disclaimer` 一致的立场，并额外点明
/// 资金流是推算的代理指标 —— 不能让人误以为「主力资金」是真实席位数据。
pub const DISCLAIMER: &str = "⚠ 本榜为盘中异动线索，非投资建议。阈值未经前瞻检验；\
「主力资金」是东财按单笔成交金额分档推算的代理指标，非真实席位数据，\
无法区分机构/游资/拆单，无法识别对倒。";

/// 渲染盘中异动推送。**纯函数**，便于测试。
pub fn render_movers(movers: &[Mover], flow_ok: bool) -> String {
    let mut s = String::new();
    if !flow_ok {
        s.push_str("> 资金流暂不可用（数据源限流），本批仅凭价量判定\n\n");
    }
    for m in movers {
        s.push_str(&format!(
            "**{} {}** {} {:+.2}% | 量能 {:.1}× | {} | {}\n",
            m.code, m.name, dir_arrow(m.jump_pct), m.jump_pct * 100.0,
            m.vol_surge_x, flow_text(m), horizon_text(m.horizon)));
        let d = divergence_text(m.divergence);
        if !d.is_empty() { s.push_str(&format!("　{}\n", d)); }
        if m.baseline == Baseline::Fallback {
            s.push_str("　（量能基准历史样本不足，已降级为当日均值）\n");
        }
    }
    s.push_str(&format!("\n{}", DISCLAIMER));
    s
}

/// 渲染收盘汇总。**纯函数**。
///
/// `close_ret` 是本项目最有价值的一列：它免费积累出信号质量的评估数据 ——
/// 异动发出后到收盘是涨是跌，数月后即可回答「这套阈值到底有没有用」。
pub fn render_summary(rows: &[SignalRow], day: NaiveDate) -> String {
    if rows.is_empty() {
        return format!("**{} 盘中异动汇总**\n\n今日无异动信号。\n\n{}", day, DISCLAIMER);
    }
    let mut s = format!("**{} 盘中异动汇总**（{} 条）\n\n", day, rows.len());
    for r in rows {
        let ret = match r.close_ret {
            Some(v) => format!("{:+.2}%", v * 100.0),
            // 「没数据」≠「零收益」，不可显示成 0.00%
            None => "结局未知".to_string(),
        };
        let flow = match r.main_net_pct {
            Some(p) => format!("主力{:+.1}%", p * 100.0),
            None => "资金流N/A".to_string(),
        };
        s.push_str(&format!(
            "- {} **{} {}** 触发 {:+.2}% @{:.2} | 量能 {:.1}× | {} | {} | 至收盘 {}\n",
            r.ts.format("%H:%M"), r.code, r.name, r.jump_pct * 100.0, r.trigger_price,
            r.vol_surge_x, flow, tag_cn(&r.horizon_tag), ret));
    }
    let known: Vec<f64> = rows.iter().filter_map(|r| r.close_ret).collect();
    if !known.is_empty() {
        let win = known.iter().filter(|v| **v > 0.0).count();
        let avg = known.iter().sum::<f64>() / known.len() as f64;
        s.push_str(&format!(
            "\n信号后至收盘：{}/{} 上涨，均值 {:+.2}%（样本 {} 条，尚不足以证明有效性）\n",
            win, known.len(), avg * 100.0, known.len()));
    }
    s.push_str(&format!("\n{}", DISCLAIMER));
    s
}

fn tag_cn(tag: &str) -> &str {
    match tag { "long" => "长线", _ => "短线" }
}

/// 全市场 A 股符号表（腾讯格式）。从既有的 universe 清单派生 ——
/// 不重复造轮子，也不硬编码股票池。
pub fn a_share_symbols(listings: &[crate::stock::data::universe::Listing]) -> Vec<String> {
    listings.iter()
        .filter(|l| !l.is_risky_shell())
        .filter_map(|l| snapshot::symbol(l.market, &l.code))
        .collect()
}

/// 跑一个抓取时点。
///
/// 编排顺序刻意如此：先落库再检测 —— 检测要读库里的历史同时点数据，
/// 且即便检测失败，快照也已安全落盘。
pub fn run_tick(
    conn: &mut Connection,
    cfg: &RealtimeCfg,
    symbols: &[String],
    names: &std::collections::HashMap<String, String>,
    now: NaiveDateTime,
) -> Result<TickOutcome> {
    let ticks = snapshot::fetch(symbols)?;
    if ticks.is_empty() { return Err(anyhow!("快照为空")) }

    // 快照自证：拿到的是今天的行情吗？不是 → 节假日，标记后当天不再重试。
    let tss: Vec<NaiveDateTime> = ticks.iter().map(|t| t.ts).collect();
    if !calendar::verify_fresh(&tss, now.date()) {
        store::mark_non_trading(conn, now.date())?;
        return Err(anyhow!("{} 非交易日（行情时间戳非今日），已标记", now.date()));
    }

    let n = store::insert_ticks(conn, &ticks)?;
    let candidates = detect(conn, cfg, &ticks, now)?;

    // 资金流：只查候选。失败不致命 —— 佐证拿不到不影响价量主判定成立。
    let secids: Vec<String> = candidates.iter()
        .filter_map(|m| secid_of(&m.code).map(|s| s.param()))
        .collect();
    let (flows, flow_ok) = match flow::fetch(&secids) {
        Ok(f) => (flow::by_code(f), true),
        Err(e) => {
            eprintln!("资金流不可用（{e}），本批仅凭价量出榜");
            (Default::default(), false)
        }
    };

    let mut out: Vec<Mover> = candidates.into_iter().map(|mut m| {
        if let Some(f) = flows.get(&m.code) {
            m.main_net = Some(f.main_net);
            m.main_net_pct = Some(f.main_net_pct);
            m.name = f.name.clone();
        } else if let Some(n) = names.get(&m.code) {
            m.name = n.clone();
        }
        m.divergence = movers::divergence(m.jump_pct, m.main_net_pct, cfg.main_flow_pct);
        m.horizon = classify_one(&m.code, now.date());
        m
    }).collect();
    out = movers::rank_top(out, usize::MAX);

    let already = store::pushed_today(conn, now.date())?;
    let pushable = select_pushable(&out, &already, cfg);
    let push_set: HashSet<&str> = pushable.iter().map(|m| m.code.as_str()).collect();
    for m in &out {
        store::insert_signal(conn, m, push_set.contains(m.code.as_str()))?;
    }
    store::prune(conn, now, cfg.retain_days)?;

    Ok(TickOutcome { ticks: n, movers: out, pushed: pushable, flow_ok })
}

fn secid_of(code: &str) -> Option<crate::stock::data::secid::Secid> {
    crate::stock::data::resolve_secid(code).ok()
}

/// 对全市场快照跑异动检测。
fn detect(
    conn: &Connection, cfg: &RealtimeCfg, ticks: &[snapshot::Tick], now: NaiveDateTime,
) -> Result<Vec<Mover>> {
    let since = now.date() - chrono::Duration::days(cfg.baseline_days);
    let slot = movers::slot_of(now);
    let mut out = Vec::new();
    for t in ticks {
        let recent = store::recent_ticks(conn, &t.code, 2)?;
        let history = store::same_slot_volumes(conn, &t.code, slot, now.date(), since)?;
        // 冷启动兜底：无历史同时点样本时用当日累计均量。
        // 当日均量 = 累计量 ÷ 已过时点数，粗糙但总比不判定强。
        let today_avg = today_average(conn, &t.code, now)?;
        let Some((jump, surge, base, ts, price)) = movers::compute(&recent, &history, today_avg) else { continue };
        if !movers::is_mover(jump, surge, cfg.price_jump_pct, cfg.volume_surge_x) { continue }
        out.push(Mover {
            code: t.code.clone(),
            name: t.code.clone(), // 稍后由 flow/names 补齐
            ts, price, jump_pct: jump, vol_surge_x: surge,
            main_net: None, main_net_pct: None,
            divergence: Divergence::Unknown,
            horizon: Horizon::Short,
            baseline: base,
        });
    }
    Ok(out)
}

/// 当日已过时点的平均每时点成交量增量。仅作冷启动兜底。
fn today_average(conn: &Connection, code: &str, now: NaiveDateTime) -> Result<Option<f64>> {
    let all = store::recent_ticks(conn, code, calendar::ticks_per_day())?;
    let today: Vec<f64> = all.iter().filter(|(ts, _, _)| ts.date() == now.date())
        .map(|(_, _, v)| *v).collect();
    if today.len() < 2 { return Ok(None) }
    // 累计量的首尾差 ÷ 间隔数
    let (max, min) = (today[0], today[today.len() - 1]);
    let spans = (today.len() - 1) as f64;
    let avg = (max - min) / spans;
    Ok(if avg > 0.0 { Some(avg) } else { None })
}

/// 短/长线标签：拉该股估值分位 + 趋势。候选只有几十只，逐只拉扛得住。
///
/// 任何一步失败都归短线 —— 无从证明有基本面支撑，就不该声称有。
fn classify_one(code: &str, today: NaiveDate) -> Horizon {
    let cache = std::path::Path::new(".cache");
    let pe_pct = (|| {
        let points = crate::stock::data::valuation::load_or_fetch(code, &cache.join("valuation"), today).ok()?;
        let cur = crate::stock::data::valuation::at_or_before(&points, today)?;
        let pe = cur.pe_ttm?;
        let hist = crate::stock::data::valuation::positive_pes(&points);
        crate::stock::data::valuation::percentile(&hist, pe)
    })();
    let trend = (|| {
        let start = today - chrono::Duration::days(800);
        let bars = crate::stock::data::cache::load_or_fetch(code, &cache.join("stock"), start, today).ok()?;
        let d = crate::stock::diagnose::diagnose(
            code.to_string(), code.to_string(), &bars,
            &crate::stock::diagnose::DiagnoseParams::default()).ok()?;
        Some(d.trend)
    })().unwrap_or_default();
    movers::classify(pe_pct, &trend, LOW_PE_PCT)
}

/// 收盘回填结局并汇总。由日线缓存算，**不依赖 ticks** ——
/// 故 raw 数据 10 天后删除不影响回溯，亦可随时补算新口径（如 T+20）。
pub fn close_summary(conn: &Connection, day: NaiveDate) -> Result<String> {
    backfill_close(conn, day)?;
    let rows = store::signals_on(conn, day)?;
    Ok(render_summary(&rows, day))
}

fn backfill_close(conn: &Connection, day: NaiveDate) -> Result<()> {
    let cache = std::path::Path::new(".cache/stock");
    for s in store::signals_missing_close(conn, day)? {
        let ret = (|| {
            let bars = crate::stock::data::cache::load_or_fetch(
                &s.code, cache, day - chrono::Duration::days(10), day).ok()?;
            let close = bars.iter().find(|b| b.date == day)?.close;
            if s.trigger_price <= 0.0 { return None }
            Some((close - s.trigger_price) / s.trigger_price)
        })();
        // ret 为 None 时写 NULL 而非 0：日线还没同步到 / 该股当日停牌，
        // 都属于「没数据」，与「零收益」是两回事
        store::backfill_outcome(conn, s.id, store::Outcome::Close, ret)?;
    }
    Ok(())
}

/// 现在是否到了收盘汇总时刻（15:00 之后的第一个 tick 循环）。
pub fn is_summary_time(now: NaiveDateTime) -> bool {
    use chrono::Timelike;
    now.time().hour() == 15 && now.time().minute() == 10
}

/// 今天（本地时区）。
pub fn today() -> NaiveDate { Local::now().date_naive() }

/// 守护侧状态：缓存全市场符号表，每日刷新一次。
///
/// 符号表来自既有的 `universe::load_or_fetch`（datacenter 源，不受 clist
/// 封禁影响），它本身按交易日缓存到 `.cache/universe_{date}.csv`，
/// 所以每天最多真正抓一次。
pub struct Daemon {
    pub cfg: RealtimeCfg,
    symbols: Vec<String>,
    names: std::collections::HashMap<String, String>,
    loaded_for: Option<NaiveDate>,
}

impl Daemon {
    pub fn new(cfg: RealtimeCfg) -> Self {
        Self { cfg, symbols: Vec::new(), names: Default::default(), loaded_for: None }
    }

    /// 全市场符号表，按天惰性加载。
    fn ensure_universe(&mut self, today: NaiveDate) -> Result<()> {
        if self.loaded_for == Some(today) && !self.symbols.is_empty() { return Ok(()) }
        let cache = std::path::Path::new(".cache");
        // universe 按「最近已收盘交易日」组织，盘中要用昨天的清单 ——
        // 今天的估值快照要等收盘后才有。清单本身（代码+名称）不受影响。
        let date = crate::stock::data::universe::latest_trade_date()
            .unwrap_or(today - chrono::Duration::days(1));
        let listings = crate::stock::data::universe::load_or_fetch(cache, date)?;
        self.symbols = a_share_symbols(&listings);
        self.names = crate::stock::data::universe::name_map(&listings);
        self.loaded_for = Some(today);
        println!("实时抓取：已加载 {} 只 A 股符号（清单日 {}）", self.symbols.len(), date);
        Ok(())
    }

    /// 跑一个 tick。返回 None 表示本轮无需动作（非抓取时点/周末/已知节假日）。
    pub fn tick(&mut self, conn: &mut Connection, now: NaiveDateTime) -> Result<Option<TickOutcome>> {
        if should_run(conn, now)?.is_some() { return Ok(None) }
        self.ensure_universe(now.date())?;
        let names = std::mem::take(&mut self.names);
        let r = run_tick(conn, &self.cfg, &self.symbols, &names, now);
        self.names = names;
        r.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 16).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }

    fn mv(code: &str, jump: f64, surge: f64, pct: Option<f64>) -> Mover {
        Mover {
            code: code.into(), name: format!("股{code}"), ts: dt(10, 0), price: 10.0,
            jump_pct: jump, vol_surge_x: surge, main_net: pct.map(|p| p * 1e8), main_net_pct: pct,
            divergence: Divergence::None, horizon: Horizon::Short, baseline: Baseline::History,
        }
    }

    fn cfg() -> RealtimeCfg { RealtimeCfg::default() }

    #[test]
    fn weak_signals_are_stored_but_not_pushed() {
        // 阈值 2%/3×，强信号需 ≥3%/4.5×。2.5% 只进库不推送
        let weak = mv("A", 0.025, 3.5, Some(0.06));
        let out = select_pushable(&[weak], &HashSet::new(), &cfg());
        assert!(out.is_empty(), "弱信号不得推送，否则一天数百条刷屏");
    }

    #[test]
    fn strong_signals_are_pushed() {
        let strong = mv("A", 0.04, 5.0, Some(0.06));
        let out = select_pushable(&[strong], &HashSet::new(), &cfg());
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn already_pushed_today_is_filtered() {
        // 同一只股票当日只推一次。already 来自 signals 表而非内存 ——
        // 守护重启后限流状态不丢
        let strong = mv("A", 0.04, 5.0, Some(0.06));
        let already: HashSet<String> = ["A".to_string()].into_iter().collect();
        assert!(select_pushable(&[strong], &already, &cfg()).is_empty());
    }

    #[test]
    fn push_is_capped_per_tick_and_ranked_by_flow() {
        // 6 只强信号 → 只推资金流占比最高的 5 只
        let ms: Vec<Mover> = (0..6).map(|i| mv(&format!("C{i}"), 0.04, 5.0, Some(i as f64 * 0.01))).collect();
        let out = select_pushable(&ms, &HashSet::new(), &cfg());
        assert_eq!(out.len(), 5, "每时点上限 5 只");
        assert_eq!(out[0].code, "C5", "资金流占比最高的排第一");
        assert!(!out.iter().any(|m| m.code == "C0"), "占比最低的被截掉");
    }

    #[test]
    fn pushable_subset_does_not_align_with_movers_order() {
        // 回归测试：pushable 是 movers 的**过滤子集**，二者顺序不对应。
        // 若调用方拿 movers.take(pushed_count)，会把「今日已推过、本该被过滤」
        // 的那只重新推出去 —— 正是 TickOutcome.pushed 存 Mover 而非数量的原因。
        //
        // 构造：TOP 资金流占比最高（排 movers 第一），但今天已推过。
        let top = mv("TOP", 0.04, 5.0, Some(0.09));
        let second = mv("SECOND", 0.04, 5.0, Some(0.01));
        let all = movers::rank_top(vec![top, second], usize::MAX);
        assert_eq!(all[0].code, "TOP", "TOP 资金流占比最高，排第一");

        let already: HashSet<String> = ["TOP".to_string()].into_iter().collect();
        let pushable = select_pushable(&all, &already, &cfg());

        assert_eq!(pushable.len(), 1);
        assert_eq!(pushable[0].code, "SECOND", "已推过的 TOP 须被过滤，只推 SECOND");
        // 这行演示了那个 bug：按数量截取会拿到 TOP —— 完全错误的那只
        assert_eq!(all.iter().take(pushable.len()).next().unwrap().code, "TOP",
            "take(n) 会拿到 TOP，证明按数量截取是错的");
    }

    #[test]
    fn render_movers_marks_flow_unavailable() {
        // 东财封禁时榜单照出，但必须说清资金流缺失 —— 不能让人以为「无背离」
        let s = render_movers(&[mv("600519", 0.04, 5.0, None)], false);
        assert!(s.contains("资金流暂不可用"), "须标注资金流不可用: {s}");
        assert!(s.contains("600519"), "榜单照常产出");
    }

    #[test]
    fn render_movers_shows_divergence() {
        let mut m = mv("600519", 0.04, 5.0, Some(-0.08));
        m.divergence = Divergence::RetailChasing;
        let s = render_movers(&[m], true);
        assert!(s.contains("散户抬轿"), "背离须显式标出，这是资金流最有信息量的用途");
    }

    #[test]
    fn render_movers_flags_fallback_baseline() {
        // 冷启动时的信号可信度低于正常信号，必须让读者看得见
        let mut m = mv("A", 0.04, 5.0, Some(0.06));
        m.baseline = Baseline::Fallback;
        assert!(render_movers(&[m], true).contains("降级"));
    }

    #[test]
    fn every_output_carries_disclaimer() {
        // 不能让人误以为「主力资金」是真实席位数据
        let s = render_movers(&[mv("A", 0.04, 5.0, Some(0.06))], true);
        assert!(s.contains("非投资建议"));
        assert!(s.contains("代理指标"), "必须点明主力资金是推算的");
        assert!(s.contains("未经前瞻检验"));
    }

    #[test]
    fn summary_shows_unknown_outcome_not_zero() {
        // 「结局未知」不能显示成 0.00% —— 那是在伪造数据
        let rows = vec![SignalRow {
            id: 1, code: "A".into(), name: "股A".into(), ts: dt(10, 0),
            trigger_price: 10.0, jump_pct: 0.04, vol_surge_x: 5.0,
            main_net_pct: None, divergence: "none".into(), horizon_tag: "short".into(),
            close_ret: None,
        }];
        let s = render_summary(&rows, NaiveDate::from_ymd_opt(2026, 7, 16).unwrap());
        assert!(s.contains("结局未知"), "缺失不可渲染成 0.00%");
        assert!(!s.contains("+0.00%"));
    }

    #[test]
    fn summary_reports_win_rate_with_sample_caveat() {
        // 这是本项目最有价值的输出：几个月后靠它回答「阈值有没有用」。
        // 但必须同时说清样本量不足，否则 2/3 会被误读成 67% 胜率
        let mk = |code: &str, ret: f64| SignalRow {
            id: 1, code: code.into(), name: "x".into(), ts: dt(10, 0),
            trigger_price: 10.0, jump_pct: 0.04, vol_surge_x: 5.0,
            main_net_pct: Some(0.06), divergence: "none".into(), horizon_tag: "short".into(),
            close_ret: Some(ret),
        };
        let rows = vec![mk("A", 0.02), mk("B", -0.01), mk("C", 0.03)];
        let s = render_summary(&rows, NaiveDate::from_ymd_opt(2026, 7, 16).unwrap());
        assert!(s.contains("2/3 上涨"), "须给出胜率: {s}");
        assert!(s.contains("尚不足以证明有效性"), "必须标注样本不足");
    }

    #[test]
    fn empty_summary_still_carries_disclaimer() {
        let s = render_summary(&[], NaiveDate::from_ymd_opt(2026, 7, 16).unwrap());
        assert!(s.contains("今日无异动"));
        assert!(s.contains("非投资建议"));
    }

    #[test]
    fn should_run_skips_weekend_and_off_hours() {
        let c = store::open_in_memory().unwrap();
        let sat = NaiveDate::from_ymd_opt(2026, 7, 18).unwrap().and_hms_opt(10, 0, 0).unwrap();
        assert_eq!(should_run(&c, sat).unwrap(), Some(Skip::Weekend));
        assert_eq!(should_run(&c, dt(12, 0)).unwrap(), Some(Skip::NotTickTime));
        assert_eq!(should_run(&c, dt(10, 0)).unwrap(), None, "周四 10:00 应抓");
    }

    #[test]
    fn should_run_skips_known_holiday_without_network() {
        // 节假日当天第一个时点自证失败后标记；后续 19 个时点必须靠这个标记
        // 免掉请求，否则等于对数据源做 19 次无谓冲击
        let c = store::open_in_memory().unwrap();
        store::mark_non_trading(&c, dt(10, 0).date()).unwrap();
        assert_eq!(should_run(&c, dt(10, 0)).unwrap(), Some(Skip::KnownHoliday));
    }

    #[test]
    fn summary_time_is_after_close() {
        assert!(is_summary_time(dt(15, 10)));
        assert!(!is_summary_time(dt(15, 0)), "15:00 还是抓取时点，不是汇总时刻");
        assert!(!is_summary_time(dt(14, 10)));
    }
}
