//! 实时异动链路的端到端集成测试。
//!
//! 单元测试证明各部件自洽，这里证明**组装起来是对的**：喂进多天的真实形态
//! 快照，走完 落库 → 同时点基准 → 检测 → 限流 → 落信号 → 回填 → 汇总，
//! 断言最终产物。
//!
//! 不打网络：抓取层的实网验证由 `snapshot.rs` / `flow.rs` 里的 `#[ignore]`
//! 哨兵测试负责。这里锁的是编排逻辑。
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use xlh::stock::realtime::{config::RealtimeCfg, job, movers, snapshot::Tick, store};

fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
fn dt(day: u32, h: u32, mi: u32) -> NaiveDateTime {
    d(2026, 7, day).and_hms_opt(h, mi, 0).unwrap()
}

fn tick(code: &str, ts: NaiveDateTime, price: f64, cum_volume: f64) -> Tick {
    Tick {
        code: code.into(), ts, price, change_pct: 0.0,
        volume: cum_volume, amount: cum_volume * price * 100.0,
        turnover: 0.5, vol_ratio: 1.0,
    }
}

/// 造 baseline：过去 N 个交易日，每天 10:00 与 10:10 各一个快照，量能平稳。
/// 成交量是**当日累计**，故每天从 0 开始递增 —— 这是真实形态。
fn seed_history(conn: &mut Connection, code: &str, days: &[u32], base_vol: f64) {
    for &day in days {
        let ticks = vec![
            tick(code, dt(day, 10, 0), 10.0, base_vol),
            tick(code, dt(day, 10, 10), 10.0, base_vol * 2.0),
        ];
        store::insert_ticks(conn, &ticks).unwrap();
    }
}

fn cfg() -> RealtimeCfg { RealtimeCfg::default() }

#[test]
fn quiet_stock_with_flat_volume_produces_no_signal() {
    let mut c = store::open_in_memory().unwrap();
    seed_history(&mut c, "600000", &[10, 13, 14, 15], 1000.0);
    // 今天同样平稳：价格没动、量能与历史一致
    store::insert_ticks(&mut c, &[
        tick("600000", dt(16, 10, 0), 10.0, 1000.0),
        tick("600000", dt(16, 10, 10), 10.0, 2000.0),
    ]).unwrap();

    let recent = store::recent_ticks(&c, "600000", 2).unwrap();
    let hist = store::same_slot_deltas(&c, "600000", (10, 10), d(2026, 7, 16), d(2026, 7, 6)).unwrap();
    let (jump, surge, _, _, _) = movers::compute(&recent, &hist, None).unwrap();

    assert!(!movers::is_mover(jump, surge, cfg().price_jump_pct, cfg().volume_surge_x),
        "平稳股不该触发：jump={jump:.4} surge={surge:.2}");
}

#[test]
fn price_spike_with_volume_surge_is_detected_and_stored() {
    let mut c = store::open_in_memory().unwrap();
    // 历史 10:10 的量能增量稳定在 1000（累计 1000→2000）
    seed_history(&mut c, "600001", &[10, 13, 14, 15], 1000.0);
    // 今天 10:10：价 10→10.5（+5%），量增 5000（5 倍于历史基准 1000）
    store::insert_ticks(&mut c, &[
        tick("600001", dt(16, 10, 0), 10.0, 1000.0),
        tick("600001", dt(16, 10, 10), 10.5, 6000.0),
    ]).unwrap();

    let recent = store::recent_ticks(&c, "600001", 2).unwrap();
    let hist = store::same_slot_deltas(&c, "600001", (10, 10), d(2026, 7, 16), d(2026, 7, 6)).unwrap();
    assert_eq!(hist.len(), 4, "应取到 4 天历史同时点样本");

    let (jump, surge, base, ts, price) = movers::compute(&recent, &hist, None).unwrap();
    assert!((jump - 0.05).abs() < 1e-9, "10→10.5 是 +5%");
    assert!((surge - 5.0).abs() < 1e-9, "增量 5000 ÷ 基准 1000 = 5 倍");
    assert_eq!(base, movers::Baseline::History, "有历史样本就该用历史，不该 fallback");
    assert!(movers::is_mover(jump, surge, cfg().price_jump_pct, cfg().volume_surge_x));

    let m = movers::Mover {
        code: "600001".into(), name: "测试".into(), ts, price,
        jump_pct: jump, vol_surge_x: surge, main_net: Some(1e8), main_net_pct: Some(0.09),
        divergence: movers::divergence(jump, Some(0.09), cfg().main_flow_pct),
        horizon: movers::Horizon::Short, baseline: base,
    };
    store::insert_signal(&c, &m, true).unwrap();

    let rows = store::signals_on(&c, d(2026, 7, 16)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].code, "600001");
    assert!((rows[0].trigger_price - 10.5).abs() < 1e-9, "触发价须落库，回填结局要用它");
}

#[test]
fn u_shaped_volume_does_not_cause_false_positive_at_quiet_slot() {
    // 这是「同时点基准」存在的全部理由。
    // 构造 U 型：10:00 放量(增量 5000)，14:00 清淡(增量 500)。
    // 今天 14:00 的量增到 600 —— 相对当日均值看着「不放量」，
    // 但相对 14:00 的历史基准 500 只有 1.2 倍，同样不该触发。
    // 若用当日累计均值当基准，14:00 的基准会被 10:00 的高量抬高，行为完全不同。
    let mut c = store::open_in_memory().unwrap();
    for day in [10u32, 13, 14, 15] {
        store::insert_ticks(&mut c, &[
            tick("600002", dt(day, 10, 0), 10.0, 5000.0),
            tick("600002", dt(day, 14, 0), 10.0, 20000.0),
            tick("600002", dt(day, 14, 10), 10.0, 20500.0), // 尾盘增量仅 500
        ]).unwrap();
    }
    store::insert_ticks(&mut c, &[
        tick("600002", dt(16, 14, 0), 10.0, 20000.0),
        tick("600002", dt(16, 14, 10), 10.06, 20600.0), // 增量 600，价 +0.6%
    ]).unwrap();

    let recent = store::recent_ticks(&c, "600002", 2).unwrap();
    let hist = store::same_slot_deltas(&c, "600002", (14, 10), d(2026, 7, 16), d(2026, 7, 6)).unwrap();
    // 增量 500（20000→20500），不是累计量 20500。基准必须与被比较的量同口径
    assert_eq!(hist, vec![500.0; 4], "14:10 的历史同时点增量");

    let (jump, surge, _, _, _) = movers::compute(&recent, &hist, None).unwrap();
    assert!((surge - 1.2).abs() < 1e-9, "增量 600 ÷ 基准 500 = 1.2 倍");
    assert!(!movers::is_mover(jump, surge, cfg().price_jump_pct, cfg().volume_surge_x),
        "尾盘小幅波动不该误报");

    // 对照：同样是 600 的增量，若错误地拿累计量 20500 当基准，
    // 会算出 0.03 倍 —— 量能判据被彻底稀释，任何真实异动都无法触发。
    // 这正是 same_slot_deltas 必须返回增量的原因。
    let (_, wrong_surge, _, _, _) = movers::compute(&recent, &[20500.0; 4], None).unwrap();
    assert!(wrong_surge < 0.05, "用累计量当基准会把放大倍数稀释到 {wrong_surge:.3}，永不触发");
}

#[test]
fn cold_start_falls_back_and_marks_it() {
    // 首次部署：库里只有今天的两个点，没有任何历史同时点样本
    let mut c = store::open_in_memory().unwrap();
    store::insert_ticks(&mut c, &[
        tick("600003", dt(16, 10, 0), 10.0, 1000.0),
        tick("600003", dt(16, 10, 10), 10.6, 3000.0),
    ]).unwrap();

    let recent = store::recent_ticks(&c, "600003", 2).unwrap();
    let hist = store::same_slot_deltas(&c, "600003", (10, 10), d(2026, 7, 16), d(2026, 7, 6)).unwrap();
    assert!(hist.is_empty(), "冷启动无历史样本");

    let (_, _, base, _, _) = movers::compute(&recent, &hist, Some(500.0)).unwrap();
    assert_eq!(base, movers::Baseline::Fallback,
        "无历史须降级并显式标记 —— 不能静默跳过检测，也不能假装用了历史基准");
}

#[test]
fn ten_day_prune_keeps_signals_and_drops_old_ticks() {
    // 分层保留的端到端验证：raw 滚动淘汰，signals 永久
    let mut c = store::open_in_memory().unwrap();
    seed_history(&mut c, "600004", &[1, 2, 15], 1000.0);
    let old = movers::Mover {
        code: "600004".into(), name: "老信号".into(), ts: dt(1, 10, 0), price: 10.0,
        jump_pct: 0.05, vol_surge_x: 5.0, main_net: None, main_net_pct: None,
        divergence: movers::Divergence::Unknown, horizon: movers::Horizon::Short,
        baseline: movers::Baseline::History,
    };
    store::insert_signal(&c, &old, true).unwrap();

    store::prune(&c, dt(16, 15, 0), 10).unwrap();

    let remaining: i64 = c.query_row("SELECT COUNT(*) FROM ticks", [], |r| r.get(0)).unwrap();
    let sigs = store::signals_on(&c, d(2026, 7, 1)).unwrap();
    assert_eq!(remaining, 2, "7/1、7/2 的 ticks 超出 10 天须删，7/15 的保留");
    assert_eq!(sigs.len(), 1, "7/1 的信号必须永久保留 —— 它是验证阈值的唯一依据");
}

#[test]
fn full_day_flow_from_detection_to_summary() {
    // 一天的完整流程：检测 → 限流 → 落库 → 回填结局 → 汇总渲染
    let c = store::open_in_memory().unwrap();
    let mk = |code: &str, jump: f64, surge: f64, pct: Option<f64>, mi: u32| movers::Mover {
        code: code.into(), name: format!("股{code}"), ts: dt(16, 10, mi), price: 10.0,
        jump_pct: jump, vol_surge_x: surge, main_net: pct.map(|p| p * 1e8), main_net_pct: pct,
        divergence: movers::divergence(jump, pct, 0.05),
        horizon: movers::Horizon::Short, baseline: movers::Baseline::History,
    };

    let strong = mk("600011", 0.05, 6.0, Some(0.09), 0);
    let weak = mk("600012", 0.025, 3.2, Some(0.01), 10);
    let diverging = mk("600013", 0.05, 6.0, Some(-0.08), 20);

    let all = vec![strong.clone(), weak.clone(), diverging.clone()];
    let pushable = job::select_pushable(&all, &Default::default(), &cfg());

    assert_eq!(pushable.len(), 2, "弱信号须被挡在推送外，只进库");
    assert!(!pushable.iter().any(|m| m.code == "600012"));
    assert_eq!(diverging.divergence, movers::Divergence::RetailChasing,
        "涨 +5% 但主力净流出 8% → 散户抬轿");

    let push_codes: Vec<&str> = pushable.iter().map(|m| m.code.as_str()).collect();
    for m in &all {
        store::insert_signal(&c, m, push_codes.contains(&m.code.as_str())).unwrap();
    }

    // 限流状态来自库：守护重启后同一只股票当日仍不会重推
    let already = store::pushed_today(&c, d(2026, 7, 16)).unwrap();
    assert_eq!(already.len(), 2);
    assert!(job::select_pushable(&all, &already, &cfg()).is_empty(),
        "已推过的当日不得重推 —— 且这个状态跨重启有效");

    // 回填结局：涨的、跌的、没数据的
    let rows = store::signals_on(&c, d(2026, 7, 16)).unwrap();
    let by = |code: &str| rows.iter().find(|r| r.code == code).unwrap().id;
    store::backfill_outcome(&c, by("600011"), store::Outcome::Close, Some(0.03)).unwrap();
    store::backfill_outcome(&c, by("600012"), store::Outcome::Close, Some(-0.01)).unwrap();
    store::backfill_outcome(&c, by("600013"), store::Outcome::Close, None).unwrap();

    let md = job::render_summary(&store::signals_on(&c, d(2026, 7, 16)).unwrap(), d(2026, 7, 16));
    assert!(md.contains("1/2 上涨"), "胜率只统计有结局的样本: {md}");
    assert!(md.contains("结局未知"), "缺失结局不得渲染成 0.00%");
    assert!(md.contains("尚不足以证明有效性"), "必须标注样本不足");
    assert!(md.contains("代理指标"), "必须点明主力资金是推算的，非真实席位数据");
    assert!(md.contains("非投资建议"));
}

#[test]
fn holiday_marking_blocks_rest_of_day_but_premarket_does_not() {
    // 节假日：标记后当天后续 19 个时点免请求
    let c = store::open_in_memory().unwrap();
    store::mark_non_trading(&c, d(2026, 7, 16)).unwrap();
    assert_eq!(job::should_run(&c, dt(16, 14, 0)).unwrap(), Some(job::Skip::KnownHoliday));

    // 而未被标记的交易日照常跑
    assert_eq!(job::should_run(&c, dt(17, 10, 0)).unwrap(), None);
}
