# 量化交易 · 计划 3b:评估流水线(基线统计、任务队列、观察期与 watchdog、成绩单、评估线程)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把计划 3a 的准入内核接成可运行的流水线:回测产出可比较的交易基线(每笔收益、胜率、连亏),后台 `trade-eval` 线程按队列跑前推回测与每日检查,观察期满足条件自动准入、实盘表现异常自动暂停,并给出四栏成绩单(样本内 / 样本外 / 模拟盘 / 实盘)与执行损耗。

**Architecture:** 纯函数(交易序列统计、watchdog 判定、成绩单聚合)与 IO(store 查询、线程调度)分离;评估线程复用计划 2b 的线程范式:独立连接、`catch_unwind`、心跳、错误只记日志。K 线加载经闭包注入,测试全离线。

**Tech Stack:** Rust 2021、rusqlite 0.31、chrono、serde_json、既有回测引擎与 `AShareExecution`。无新依赖、不引入 tokio。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md` §10.5(持续监控)、§10.6(成绩单)、§11(运行时)、§14(任务 7、8)。前置:计划 1、2a、2b、3a 已合并入 main。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有查询按 `user_id` 隔离(含新的任务队列与统计查询);跨用户视为不存在
- 时间 `NaiveDateTime` 本地(`%Y-%m-%d %H:%M:%S`),日期 `%Y-%m-%d`
- 状态转换一律条件 UPDATE + 事件表,并与其副作用在同一事务内
- 评估线程:`std::thread`,独立 SQLite 连接(WAL),每轮 `catch_unwind`,任何错误只记日志,永不退出;写心跳 `trade-eval`
- 观察期与 watchdog 阈值取 `[trade.admission]`;新增阈值一律带范围校验
- 测试不得访问网络:K 线经闭包注入,报价经 trait 注入
- 不引入新依赖
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:164`、`src/ai.rs:170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 不使用 `git stash`

### 相对 spec 的实现细化(执行者照此实现)

1. **模拟盘 / 实盘回撤**用「累计已实现盈亏曲线」的最大回撤近似(没有逐日市值快照);在文档注释中写明该近似。
2. **每笔收益率** = `realized_pnl / 成本`,成本 = `成交价 × 数量 − realized_pnl − 费用`(与 `trade_fills` 已存字段自洽)。
3. **胜率显著性**:`σ = sqrt(p(1−p)/n)`,`p` 取回测胜率,`n` 取观察窗口笔数;不足 5 笔不判定。
4. **执行损耗** = 同一信号下「实盘成交价 ÷ 模拟盘成交价 − 1」(买入为正表示多付,卖出取反),按笔取中位数。
5. **日线策略信号**与**股票推荐迁移**仍在计划 3c;本计划只把准入状态接入 `service::submit_signal` 的调用方式(提供 `admission_for`,不改 signal 生成)。
6. **月度重跑**:`trade-eval` 线程在每月首个交易日为 `Admitted`/`Paper` 策略入队 `walk_forward` 任务。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/stock/trade_stats.rs` | 修改 | 抽出 FIFO 逐笔收益:`round_trips(trades) -> Vec<RoundTrip>` |
| `src/trade/admission/walk_forward.rs` | 修改 | 逐笔基线指标进入 `CodeMetrics` / `PoolMetrics`;进度回调;metric 校验 |
| `src/trade/admission/judge.rs` | 修改 | `BacktestBaseline::from_pool`;watchdog 判定 |
| `src/trade/admission/state.rs` | 修改 | 合法转换表;观察期通过 / watchdog 暂停的封装 |
| `src/trade/store.rs` | 修改 | `trade_eval_jobs` 表与队列读写;策略定义校验 |
| `src/trade/admission/stats.rs` | 新建 | 从成交记录算模拟盘 / 实盘统计与执行损耗 |
| `src/trade/admission/scorecard.rs` | 新建 | 四栏成绩单聚合 |
| `src/trade/admission/worker.rs` | 新建 | 单次评估任务执行(可离线测试) |
| `src/trade/daemon.rs` | 修改 | `trade-eval` 线程、每日与每月调度 |
| `src/trade/config.rs` | 修改 | `[trade.eval]` 段与新阈值 |

---

### Task 1: 逐笔收益基线(交易统计 + 回测指标)

**Files:**
- Modify: `src/stock/trade_stats.rs`、`src/trade/admission/walk_forward.rs`、`src/trade/admission/judge.rs`
- Test: 同文件 `mod tests`

**Interfaces:**
- Produces:

```rust
// stock/trade_stats.rs
pub struct RoundTrip { pub shares: f64, pub cost: f64, pub proceeds: f64, pub pnl: f64 } // Copy, PartialEq
impl RoundTrip { pub fn ret(&self) -> f64 }            // pnl / cost,cost <= 0 时为 0
pub fn round_trips(trades: &[TradeRecord]) -> Vec<RoundTrip>;   // FIFO;`trade_stats` 改为基于它聚合
// walk_forward.rs
pub struct TradeBaseline { pub avg_return: f64, pub return_sd: f64, pub win_rate: f64, pub max_consecutive_losses: usize, pub count: usize } // Serialize, PartialEq
pub fn trade_baseline(returns: &[f64]) -> TradeBaseline;         // 纯函数
// CodeMetrics 增加 pub trade_baseline: TradeBaseline
// PoolMetrics 增加 pub trade_baseline: TradeBaseline(逐项:avg/sd/win_rate 取中位数,max_consecutive_losses 取最大,count 求和)
// judge.rs
impl BacktestBaseline { pub fn from_pool(m: &PoolMetrics) -> Self }  // avg_trade_return / trade_return_sd 取自 PoolMetrics.trade_baseline,max_drawdown 取 oos_max_drawdown
```

- [ ] **Step 1: 写失败测试**

`src/stock/trade_stats.rs` 的 `mod tests` 追加:

```rust
    #[test]
    fn round_trips_report_cost_and_return_per_sell() {
        // 买 100 @10 费 5 → 每股成本 10.05;卖 100 @12 费 6 → pnl = 1200 - 6 - 1005 = 189
        let trades = vec![
            TradeRecord { date: d(2024, 1, 2), direction: Direction::Buy, shares: 100.0, price: 10.0, fee: 5.0 },
            TradeRecord { date: d(2024, 1, 3), direction: Direction::Sell, shares: 100.0, price: 12.0, fee: 6.0 },
        ];
        let rts = round_trips(&trades);
        assert_eq!(rts.len(), 1);
        assert!((rts[0].cost - 1005.0).abs() < 1e-9);
        assert!((rts[0].pnl - 189.0).abs() < 1e-9);
        assert!((rts[0].ret() - 189.0 / 1005.0).abs() < 1e-9);
        // 聚合口径与既有 trade_stats 一致
        let st = trade_stats(&trades);
        assert_eq!((st.round_trips, st.wins), (1, 1));
        assert!((st.realized_pnl - 189.0).abs() < 1e-9);
    }

    #[test]
    fn round_trips_without_lots_are_ignored_for_cost() {
        // 无持仓直接卖:成本 0,收益率按 0 处理,但仍计一次 round trip(与既有统计一致)
        let trades = vec![TradeRecord { date: d(2024, 1, 2), direction: Direction::Sell, shares: 100.0, price: 12.0, fee: 6.0 }];
        let rts = round_trips(&trades);
        assert_eq!(rts.len(), 1);
        assert_eq!(rts[0].ret(), 0.0);
    }
```

(若测试模块没有 `d(...)` / `TradeRecord` 导入,按文件内既有写法补齐。)

`src/trade/admission/walk_forward.rs` 的 `mod tests` 追加:

```rust
    #[test]
    fn trade_baseline_summarises_returns() {
        let b = trade_baseline(&[0.10, -0.05, 0.20, -0.02, -0.03]);
        assert_eq!((b.count, b.max_consecutive_losses), (5, 2));
        assert!((b.win_rate - 0.4).abs() < 1e-9);
        assert!((b.avg_return - 0.04).abs() < 1e-9);
        // 总体标准差:sqrt(Σ(x-μ)²/n)
        let var = [0.10, -0.05, 0.20, -0.02, -0.03]
            .iter()
            .map(|x| (x - 0.04) * (x - 0.04))
            .sum::<f64>()
            / 5.0;
        assert!((b.return_sd - var.sqrt()).abs() < 1e-9);
        let empty = trade_baseline(&[]);
        assert_eq!((empty.count, empty.max_consecutive_losses), (0, 0));
        assert_eq!((empty.avg_return, empty.return_sd, empty.win_rate), (0.0, 0.0, 0.0));
    }

    #[test]
    fn code_metrics_carry_trade_baseline_from_oos_windows() {
        let prices = wave_prices(190);
        let b = bars(d(2024, 1, 1), &prices);
        let out = run_code("trend", "600000", &b, &grid(), &cfg()).unwrap();
        let m = out.metrics();
        assert_eq!(m.trade_baseline.count, m.oos_trades.min(m.trade_baseline.count), "逐笔基线来自样本外成交");
        assert!(m.trade_baseline.count > 0, "锯齿行情应产生成交");
        assert!(m.trade_baseline.win_rate >= 0.0 && m.trade_baseline.win_rate <= 1.0);
    }

    #[test]
    fn pool_baseline_takes_medians_and_worst_streak() {
        let mk = |avg: f64, sd: f64, wr: f64, streak: usize, n: usize| TradeBaseline {
            avg_return: avg,
            return_sd: sd,
            win_rate: wr,
            max_consecutive_losses: streak,
            count: n,
        };
        let mut a = sample_code_metrics("a", 0.1);
        a.trade_baseline = mk(0.02, 0.01, 0.5, 2, 10);
        let mut b = sample_code_metrics("b", 0.2);
        b.trade_baseline = mk(0.04, 0.03, 0.7, 5, 20);
        let p = aggregate(vec![a, b]);
        assert!((p.trade_baseline.avg_return - 0.03).abs() < 1e-9, "中位数");
        assert_eq!(p.trade_baseline.max_consecutive_losses, 5, "取最差");
        assert_eq!(p.trade_baseline.count, 30, "求和");
    }
```

> `sample_code_metrics(code, ret)` 是本测试模块内的小助手:构造一个 `CodeMetrics`,除 `code`/`oos_return` 外其余填 0 / 空(若模块内已有等价助手则复用)。

`src/trade/admission/judge.rs` 的 `mod tests` 追加:

```rust
    #[test]
    fn baseline_is_derived_from_pool_metrics() {
        let mut m = pool(1.0, 0.20, 40, 0.60, 4.0, 0.30, 1.4);
        m.trade_baseline.avg_return = 0.03;
        m.trade_baseline.return_sd = 0.01;
        let b = BacktestBaseline::from_pool(&m);
        assert!((b.avg_trade_return - 0.03).abs() < 1e-9);
        assert!((b.trade_return_sd - 0.01).abs() < 1e-9);
        assert!((b.max_drawdown - 0.20).abs() < 1e-9);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib stock::trade_stats::tests trade::admission`
Expected: 编译失败(`round_trips`、`TradeBaseline`、`from_pool` 未定义)

- [ ] **Step 3: 实现 trade_stats.rs**

```rust
/// 一次卖出对应的 FIFO 回合。买入费用摊入每股成本,卖出费用从收入中扣除。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct RoundTrip {
    pub shares: f64,
    /// 匹配到的买入成本(含买入费)
    pub cost: f64,
    /// 卖出收入(已扣卖出费)
    pub proceeds: f64,
    pub pnl: f64,
}

impl RoundTrip {
    /// 本回合收益率;无成本(裸卖)返回 0。
    pub fn ret(&self) -> f64 {
        if self.cost > 1e-9 {
            self.pnl / self.cost
        } else {
            0.0
        }
    }
}

/// FIFO 还原每次卖出的成本与盈亏。`trade_stats` 在其之上聚合。
pub fn round_trips(trades: &[TradeRecord]) -> Vec<RoundTrip> {
    let mut lots: std::collections::VecDeque<(f64, f64)> = std::collections::VecDeque::new();
    let mut out = Vec::new();
    for t in trades {
        match t.direction {
            Direction::Buy => {
                if t.shares > 1e-9 {
                    lots.push_back((t.shares, t.price + t.fee / t.shares));
                }
            }
            Direction::Sell => {
                let mut remaining = t.shares;
                let mut cost = 0.0;
                while remaining > 1e-9 {
                    let Some((lot_shares, lot_cost)) = lots.front().copied() else {
                        break;
                    };
                    let take = remaining.min(lot_shares);
                    cost += take * lot_cost;
                    let left = lot_shares - take;
                    if left > 1e-9 {
                        lots.front_mut().expect("刚读到队首").0 = left;
                    } else {
                        lots.pop_front();
                    }
                    remaining -= take;
                }
                let matched = t.shares - remaining;
                let proceeds = matched * t.price - t.fee;
                out.push(RoundTrip {
                    shares: matched,
                    cost,
                    proceeds,
                    pnl: proceeds - cost,
                });
            }
        }
    }
    out
}
```

`trade_stats` 改为基于 `round_trips` 聚合(逐项含义不变):`round_trips = rts.len()`,`wins` 为 `pnl > 0` 的个数,`gross_win`/`gross_loss` 分别累加正负 `pnl`,其余公式照旧。既有测试期望值不得改变。

- [ ] **Step 4: 实现 walk_forward.rs 与 judge.rs**

1. `use crate::stock::trade_stats::round_trips;`
2. 新增:

```rust
/// 逐笔收益的基线统计。观察期与 watchdog 都与它比较。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TradeBaseline {
    pub avg_return: f64,
    /// 总体标准差
    pub return_sd: f64,
    pub win_rate: f64,
    pub max_consecutive_losses: usize,
    pub count: usize,
}

pub fn trade_baseline(returns: &[f64]) -> TradeBaseline {
    if returns.is_empty() {
        return TradeBaseline::default();
    }
    let n = returns.len() as f64;
    let avg = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - avg) * (r - avg)).sum::<f64>() / n;
    let wins = returns.iter().filter(|r| **r > 0.0).count();
    let mut streak = 0usize;
    let mut worst = 0usize;
    for r in returns {
        if *r <= 0.0 {
            streak += 1;
            worst = worst.max(streak);
        } else {
            streak = 0;
        }
    }
    TradeBaseline {
        avg_return: avg,
        return_sd: var.sqrt(),
        win_rate: wins as f64 / n,
        max_consecutive_losses: worst,
        count: returns.len(),
    }
}
```

3. `WindowResult` 增加 `pub oos_returns: Vec<f64>`(不参与 `Serialize`:`WindowResult` 本就未派生 Serialize),在 `run_code` 中由 `round_trips(&oos.trades).iter().map(|rt| rt.ret()).collect()` 填充。
4. `CodeMetrics` 增加 `pub trade_baseline: TradeBaseline`,在 `metrics()` 中用所有窗口 `oos_returns` 拼接后调用 `trade_baseline`。
5. `PoolMetrics` 增加 `pub trade_baseline: TradeBaseline`,在 `aggregate` 中:avg/sd/win_rate 取中位数,`max_consecutive_losses` 取最大,`count` 求和。
6. `judge.rs`:

```rust
impl BacktestBaseline {
    /// 从池内回测指标取基线:逐笔收益均值与标准差来自 `trade_baseline`,回撤取样本外最大回撤。
    pub fn from_pool(m: &PoolMetrics) -> Self {
        Self {
            avg_trade_return: m.trade_baseline.avg_return,
            trade_return_sd: m.trade_baseline.return_sd,
            max_drawdown: m.oos_max_drawdown,
        }
    }
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib stock::trade_stats::tests trade::admission`
Expected: 全部 PASS(trade_stats +2、walk_forward +3、judge +1;既有测试值不变)

- [ ] **Step 6: Commit**

```bash
git add src/stock/trade_stats.rs src/trade/admission
git commit -m "feat(trade): 逐笔回合统计与回测交易基线(均值、标准差、胜率、连亏)"
```

---

### Task 2: 计划 3a 遗留阻塞项

**Files:**
- Modify: `src/trade/admission/state.rs`、`src/trade/admission/walk_forward.rs`、`src/trade/store.rs`、`src/trade/config.rs`
- Test: 同文件 `mod tests`

**Interfaces:**

```rust
// state.rs
pub fn is_legal_transition(from: StrategyStatus, to: StrategyStatus) -> bool;  // 见下表
// walk_forward.rs
pub struct PoolProgress<'a> { pub on_code: &'a mut dyn FnMut(&str, usize, usize) -> bool }  // 返回 false 表示取消
pub fn run_pool<F>(kind, pool, grid, cfg, load: F, progress: Option<&mut PoolProgress>) -> Result<PoolMetrics>;
pub fn validate_metric(metric: &str) -> Result<()>;   // 未知 metric 报错;WalkForwardCfg 使用处调用
// store.rs
pub fn validate_new_strategy(s: &NewStrategy) -> Result<()>;  // 非空名称、非空池、代码 6 位数字且不重复、kind 已知、grid_toml 可解析且非空
```

合法转换表(其余一律非法):

| from | to |
|---|---|
| Draft | Backtesting、Paper(仅 mover) |
| Backtesting | Paper、Failed |
| Failed | Backtesting、Paper(仅 mover) |
| Paper | Admitted、Failed、Suspended |
| Admitted | Suspended |
| Suspended | Backtesting、Paper(仅 mover) |

- [ ] **Step 1: 写失败测试**

`state.rs` 追加:

```rust
    #[test]
    fn illegal_transitions_are_refused() {
        let c = db();
        let id = strategy(&c, "rsi");
        assert!(!is_legal_transition(StrategyStatus::Draft, StrategyStatus::Admitted));
        assert!(is_legal_transition(StrategyStatus::Paper, StrategyStatus::Admitted));
        assert_eq!(
            update_status(&c, 1, id, StrategyStatus::Draft, StrategyStatus::Admitted, "越权", at(16, 9, 1)).unwrap(),
            Transition::AlreadyHandled,
            "非法转换不得写库"
        );
        assert_eq!(store::get_strategy(&c, 1, id).unwrap().unwrap().status, StrategyStatus::Draft);
        assert!(store::list_status_events(&c, 1, id).unwrap().is_empty());
    }
```

(注意:测试里既有的 `update_status(.., Draft, Paper, ..)` 调用改为 mover 策略或改走 `submit_for_backtest`,因为 Draft → Paper 仅对 mover 合法;按实际情况调整并在报告说明。)

`walk_forward.rs` 追加:

```rust
    #[test]
    fn progress_reports_each_code_and_cancels() {
        let prices = wave_prices(190);
        let good = bars(d(2024, 1, 1), &prices);
        let mut seen: Vec<(String, usize, usize)> = Vec::new();
        let mut cb = |code: &str, done: usize, total: usize| {
            seen.push((code.to_string(), done, total));
            done < 1 // 第一只之后取消
        };
        let mut p = PoolProgress { on_code: &mut cb };
        let m = run_pool(
            "trend",
            &["600000".into(), "000001".into()],
            &grid(),
            &cfg(),
            |_| Ok(good.clone()),
            Some(&mut p),
        )
        .unwrap();
        assert_eq!(seen.len(), 1, "取消后不再继续");
        assert_eq!(m.codes.len(), 1);
        assert!(m.skipped.iter().any(|(c, r)| c == "000001" && r.contains("取消")));
    }

    #[test]
    fn unknown_metric_is_rejected() {
        assert!(validate_metric("sharpe").is_ok());
        assert!(validate_metric("sharp").is_err());
    }
```

`store.rs` 追加:

```rust
    #[test]
    fn new_strategy_is_validated() {
        let c = db();
        let mut s = new_strategy();
        s.pool = vec!["600000".into(), "600000".into()];
        assert!(create_strategy(&c, &s, at(16, 9, 0)).is_err(), "重复代码");
        let mut s = new_strategy();
        s.pool = Vec::new();
        assert!(create_strategy(&c, &s, at(16, 9, 0)).is_err(), "空池");
        let mut s = new_strategy();
        s.kind = "nope".into();
        assert!(create_strategy(&c, &s, at(16, 9, 0)).is_err(), "未知策略类型");
        let mut s = new_strategy();
        s.grid_toml = "rsi_window = ".into();
        assert!(create_strategy(&c, &s, at(16, 9, 0)).is_err(), "网格无法解析");
        assert!(create_strategy(&c, &new_strategy(), at(16, 9, 0)).is_ok());
    }
```

- [ ] **Step 2: 运行确认失败 → Step 3: 实现**

1. `state.rs`:

```rust
/// 合法转换表(spec §10.2)。`mover` 的 Draft/Failed/Suspended → Paper 由 `submit_for_backtest` 走这里。
pub fn is_legal_transition(from: StrategyStatus, to: StrategyStatus) -> bool {
    use StrategyStatus::*;
    matches!(
        (from, to),
        (Draft, Backtesting)
            | (Draft, Paper)
            | (Backtesting, Paper)
            | (Backtesting, Failed)
            | (Failed, Backtesting)
            | (Failed, Paper)
            | (Paper, Admitted)
            | (Paper, Failed)
            | (Paper, Suspended)
            | (Admitted, Suspended)
            | (Suspended, Backtesting)
            | (Suspended, Paper)
    )
}
```
`transition_status` 开头加:`if !is_legal_transition(expect, to) { return Ok(Transition::AlreadyHandled); }`,并在文档注释说明「非法转换不写库、不写事件」。

2. `walk_forward.rs`:

```rust
/// 池内进度回调。`on_code(code, 已完成, 总数)` 返回 false 表示取消,剩余代码记为「已取消」。
pub struct PoolProgress<'a> {
    pub on_code: &'a mut dyn FnMut(&str, usize, usize) -> bool,
}

/// 训练窗选参依据必须是已知指标,否则静默回退到 sharpe 会让配置形同虚设。
pub fn validate_metric(metric: &str) -> Result<()> {
    match metric {
        "sharpe" | "total_return" | "annualized" | "max_drawdown" => Ok(()),
        other => Err(anyhow!(
            "未知选参指标 {other};可选 sharpe | total_return | annualized | max_drawdown"
        )),
    }
}
```

`run_code` 开头加 `validate_metric(&cfg.metric)?;`。`run_pool` 签名末尾增加 `progress: Option<&mut PoolProgress>`,循环体改为:

```rust
    let total = pool.len();
    let mut progress = progress;
    for (done, code) in pool.iter().enumerate() {
        // ...原有 load / run_code / skipped 逻辑不变...
        if let Some(p) = progress.as_deref_mut() {
            if !(p.on_code)(code, done + 1, total) {
                for rest in pool.iter().skip(done + 1) {
                    skipped.push((rest.clone(), "已取消".to_string()));
                }
                break;
            }
        }
    }
```

3. `store.rs`:

```rust
/// 策略定义校验:名称、股票池、类型与网格都要在入库前拦住,
/// 否则错误要等到跑完整轮回测才以「未通过」的形式暴露。
pub fn validate_new_strategy(s: &NewStrategy) -> Result<()> {
    if s.name.trim().is_empty() {
        return Err(anyhow!("策略名称不能为空"));
    }
    if !matches!(
        s.kind.as_str(),
        "dca" | "smart_dca" | "trend" | "rsi" | "adaptive" | "mover"
    ) {
        return Err(anyhow!("未知策略类型: {}", s.kind));
    }
    if s.pool.is_empty() {
        return Err(anyhow!("股票池不能为空"));
    }
    let mut seen = std::collections::HashSet::new();
    for code in &s.pool {
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(anyhow!("股票代码须为 6 位数字: {code}"));
        }
        if !seen.insert(code.as_str()) {
            return Err(anyhow!("股票池含重复代码: {code}"));
        }
    }
    let grid: toml::Table = s
        .grid_toml
        .parse()
        .map_err(|e| anyhow!("参数网格解析失败: {e}"))?;
    if grid.is_empty() {
        return Err(anyhow!("参数网格不能为空"));
    }
    Ok(())
}
```

在 `create_strategy` 与 `update_definition` 的第一行调用 `validate_new_strategy(s)?;`。

- [ ] **Step 4: 运行确认通过 + Commit**

```bash
git add src/trade
git commit -m "fix(trade): 状态转换合法性表、池内进度与取消、策略定义与 metric 校验"
```

---

### Task 3: 评估任务队列

**Files:**
- Modify: `src/trade/store.rs`、`src/trade/model.rs`
- Test: `src/trade/store.rs` 内 `mod tests`

**Interfaces:**

```rust
// model.rs
pub enum EvalKind { WalkForward, PaperCheck, Watchdog }   // as_str/parse
pub enum JobStatus { Queued, Running, Done, Failed }      // as_str/parse
pub struct EvalJob { pub id: i64, pub user_id: i64, pub strategy_id: i64, pub kind: EvalKind, pub status: JobStatus, pub progress: Option<String>, pub error: Option<String>, pub created_at: NaiveDateTime }
// store.rs
pub fn enqueue_eval(conn, user_id, strategy_id, kind, now) -> Result<Option<i64>>; // 同策略同类型已排队 → None
pub fn claim_next_job(conn, now) -> Result<Option<EvalJob>>;                        // 最早的 queued → running(条件 UPDATE)
pub fn set_job_progress(conn, job_id, progress: &str, now) -> Result<()>;
pub fn finish_job(conn, job_id, error: Option<&str>, now) -> Result<()>;            // done / failed
pub fn list_jobs(conn, user_id, limit: usize) -> Result<Vec<EvalJob>>;
```

表:

```sql
CREATE TABLE IF NOT EXISTS trade_eval_jobs (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id     INTEGER NOT NULL,
  strategy_id INTEGER NOT NULL,
  kind        TEXT NOT NULL,
  status      TEXT NOT NULL,
  progress    TEXT,
  error       TEXT,
  created_at  TEXT NOT NULL,
  started_at  TEXT,
  finished_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_trade_eval_jobs_status ON trade_eval_jobs(status, id);
```

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn eval_queue_dedups_claims_and_finishes() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        let job = enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 1)).unwrap().unwrap();
        assert!(enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 2)).unwrap().is_none(), "同类型已排队");
        assert!(enqueue_eval(&c, 1, id, EvalKind::Watchdog, at(16, 9, 2)).unwrap().is_some(), "不同类型可并存");

        let claimed = claim_next_job(&c, at(16, 9, 3)).unwrap().unwrap();
        assert_eq!((claimed.id, claimed.status), (job, JobStatus::Running));
        set_job_progress(&c, job, "3/10", at(16, 9, 4)).unwrap();
        finish_job(&c, job, None, at(16, 9, 5)).unwrap();
        let jobs = list_jobs(&c, 1, 10).unwrap();
        let done = jobs.iter().find(|j| j.id == job).unwrap();
        assert_eq!((done.status, done.progress.as_deref()), (JobStatus::Done, Some("3/10")));
        assert!(list_jobs(&c, 2, 10).unwrap().is_empty(), "用户隔离");

        // 完成后可再次入队;失败记录原因
        let again = enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 6)).unwrap().unwrap();
        let claimed = claim_next_job(&c, at(16, 9, 7)).unwrap().unwrap();
        assert_ne!(claimed.id, job);
        finish_job(&c, again, Some("加载失败"), at(16, 9, 8)).unwrap();
        let jobs = list_jobs(&c, 1, 10).unwrap();
        let failed = jobs.iter().find(|j| j.id == again).unwrap();
        assert_eq!(failed.status, JobStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("加载失败"));
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::store::tests trade::model::tests`
Expected: 编译失败(`EvalKind`、`enqueue_eval` 等未定义)

- [ ] **Step 3: 实现 model.rs**

```rust
/// 评估任务类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalKind {
    /// 滚动前推回测
    WalkForward,
    /// 观察期检查
    PaperCheck,
    /// 实盘表现监控
    Watchdog,
}

impl EvalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvalKind::WalkForward => "walk_forward",
            EvalKind::PaperCheck => "paper_check",
            EvalKind::Watchdog => "watchdog",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "walk_forward" => EvalKind::WalkForward,
            "paper_check" => EvalKind::PaperCheck,
            "watchdog" => EvalKind::Watchdog,
            _ => return Err(anyhow!("未知评估任务类型: {s}")),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => JobStatus::Queued,
            "running" => JobStatus::Running,
            "done" => JobStatus::Done,
            "failed" => JobStatus::Failed,
            _ => return Err(anyhow!("未知任务状态: {s}")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvalJob {
    pub id: i64,
    pub user_id: i64,
    pub strategy_id: i64,
    pub kind: EvalKind,
    pub status: JobStatus,
    pub progress: Option<String>,
    pub error: Option<String>,
    pub created_at: NaiveDateTime,
}
```

在 model 测试的枚举往返用例中加入 `EvalKind` 与 `JobStatus`。

- [ ] **Step 4: 实现 store.rs**

`SCHEMA` 追加本任务开头的建表与索引;`migrate_is_idempotent_and_creates_tables` 的表数由 11 改为 12。追加:

```rust
const JOB_COLS: &str = "id, user_id, strategy_id, kind, status, progress, error, created_at";

fn read_job(r: &Row) -> rusqlite::Result<(i64, i64, i64, String, String, Option<String>, Option<String>, String)> {
    Ok((
        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
    ))
}

#[allow(clippy::type_complexity)]
fn to_job(
    raw: (i64, i64, i64, String, String, Option<String>, Option<String>, String),
) -> Result<EvalJob> {
    let (id, user_id, strategy_id, kind, status, progress, error, created_at) = raw;
    Ok(EvalJob {
        id,
        user_id,
        strategy_id,
        kind: EvalKind::parse(&kind)?,
        status: JobStatus::parse(&status)?,
        progress,
        error,
        created_at: parse_ts(&created_at)?,
    })
}

/// 入队一个评估任务;同策略同类型已排队 / 运行中则返回 None(幂等)。
pub fn enqueue_eval(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    kind: EvalKind,
    now: NaiveDateTime,
) -> Result<Option<i64>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM trade_eval_jobs
           WHERE user_id = ?1 AND strategy_id = ?2 AND kind = ?3 AND status IN ('queued', 'running'))",
        params![user_id, strategy_id, kind.as_str()],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(None);
    }
    conn.execute(
        "INSERT INTO trade_eval_jobs (user_id, strategy_id, kind, status, created_at)
         VALUES (?1, ?2, ?3, 'queued', ?4)",
        params![user_id, strategy_id, kind.as_str(), fmt_ts(now)],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// 领取最早的排队任务并置为运行中。无任务返回 None。
pub fn claim_next_job(conn: &Connection, now: NaiveDateTime) -> Result<Option<EvalJob>> {
    conn.query_row(
        &format!(
            "UPDATE trade_eval_jobs SET status = 'running', started_at = ?1
             WHERE id = (SELECT id FROM trade_eval_jobs WHERE status = 'queued' ORDER BY id LIMIT 1)
             RETURNING {JOB_COLS}"
        ),
        [fmt_ts(now)],
        read_job,
    )
    .optional()?
    .map(to_job)
    .transpose()
}

pub fn set_job_progress(
    conn: &Connection,
    job_id: i64,
    progress: &str,
    _now: NaiveDateTime,
) -> Result<()> {
    conn.execute(
        "UPDATE trade_eval_jobs SET progress = ?1 WHERE id = ?2",
        params![progress, job_id],
    )?;
    Ok(())
}

/// 结束任务:`error` 为 None 记 done,否则记 failed。
pub fn finish_job(
    conn: &Connection,
    job_id: i64,
    error: Option<&str>,
    now: NaiveDateTime,
) -> Result<()> {
    let status = if error.is_some() {
        JobStatus::Failed
    } else {
        JobStatus::Done
    };
    conn.execute(
        "UPDATE trade_eval_jobs SET status = ?1, error = ?2, finished_at = ?3 WHERE id = ?4",
        params![status.as_str(), error, fmt_ts(now), job_id],
    )?;
    Ok(())
}

pub fn list_jobs(conn: &Connection, user_id: i64, limit: usize) -> Result<Vec<EvalJob>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLS} FROM trade_eval_jobs WHERE user_id = ?1 ORDER BY id DESC LIMIT ?2"
    ))?;
    let raws = stmt
        .query_map(params![user_id, limit as i64], read_job)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(to_job).collect()
}
```

顶部 use 增加 `EvalJob, EvalKind, JobStatus`。

- [ ] **Step 4b: 运行确认通过**

Run: `cargo test --lib trade::store::tests trade::model::tests`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src/trade/store.rs src/trade/model.rs
git commit -m "feat(trade): 评估任务队列(入队去重、领取、进度与结果)"
```

---

### (以下任务移至计划 3c:模拟盘/实盘统计与 watchdog、成绩单与任务执行、trade-eval 线程)

<!-- 原 Task 4/5/6 的草案保留在此供计划 3c 起草时参考,不作为本计划的执行内容。

### 草案:模拟盘 / 实盘统计与 watchdog

**Files:**
- Create: `src/trade/admission/stats.rs`
- Modify: `src/trade/admission/mod.rs`、`src/trade/admission/judge.rs`、`src/trade/config.rs`
- Test: 新文件与 judge 内 `mod tests`

**Interfaces:**

```rust
// stats.rs
pub struct FillRow { pub account: Account, pub code: String, pub side: Direction, pub price: f64, pub qty: u64, pub fee: f64, pub realized_pnl: Option<f64>, pub filled_at: NaiveDateTime, pub signal_id: i64 }
pub fn strategy_fills(conn: &Connection, user_id: i64, strategy_id: i64, account: Account, since: Option<NaiveDateTime>) -> Result<Vec<FillRow>>;
pub fn trade_returns(fills: &[FillRow]) -> Vec<f64>;         // 卖出成交:realized / 成本;成本 = price*qty − realized − fee
pub fn equity_drawdown(returns_pnl: &[f64]) -> f64;          // 累计已实现盈亏曲线的最大回撤(相对峰值)
pub fn paper_stats(conn, user_id, strategy_id, since: NaiveDateTime, now: NaiveDateTime) -> Result<PaperStats>; // days = 自 since 起的自然交易日跨度(工作日计数)
pub fn execution_loss(conn, user_id, strategy_id) -> Result<Option<f64>>;  // 同 signal 的实盘价 ÷ 模拟盘价 − 1,买入为正,卖出取反,取中位数
// judge.rs
pub struct WatchdogStats { pub drawdown: f64, pub recent_win_rate: f64, pub recent_trades: usize, pub max_consecutive_losses: usize }
pub fn judge_watchdog(s: &WatchdogStats, baseline: &BacktestBaseline, backtest_win_rate: f64, backtest_max_streak: usize, cfg: &AdmissionCfg) -> Option<String>; // Some(原因) → 应暂停
// config.rs AdmissionCfg 增加:drawdown_multiple(1.5)、win_rate_sigma(2.0)、streak_multiple(1.5)、watchdog_window(20)
```

判定(spec §10.5):回撤 > 回测最大回撤 × `drawdown_multiple`;近 `watchdog_window` 笔胜率 < 回测胜率 − `win_rate_sigma` × σ(σ = sqrt(p(1−p)/n),n < 5 不判);连亏 > 回测最长连亏 × `streak_multiple`。

- [ ] **Step 1: 写失败测试**

`stats.rs`(新建,先写测试):

```rust
//! 从成交记录还原模拟盘 / 实盘表现:逐笔收益、回撤、执行损耗。
//! 说明:没有逐日市值快照,回撤用「累计已实现盈亏曲线」的最大回撤近似。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{Account, NewStrategy};
    use crate::trade::store;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime { /* 同其他模块 */ }

    #[test]
    fn trade_returns_use_cost_derived_from_fill_fields() {
        // 卖出 1000 @11,费 10.61,realized 984.29 → 成本 = 11000 − 984.29 − 10.61 = 10005.1
        let f = FillRow { account: Account::Real, code: "600000".into(), side: Direction::Sell, price: 11.0, qty: 1000, fee: 10.61, realized_pnl: Some(984.29), filled_at: at(16, 10, 0), signal_id: 1 };
        let r = trade_returns(&[f]);
        assert_eq!(r.len(), 1);
        assert!((r[0] - 984.29 / 10_005.1).abs() < 1e-9, "{}", r[0]);
        // 买入不计入
        let b = FillRow { side: Direction::Buy, realized_pnl: None, ..f.clone() };
        assert!(trade_returns(&[b]).is_empty());
    }

    #[test]
    fn drawdown_of_cumulative_pnl() {
        // 累计:100, 60, 160, 110 → 峰值 100 后回撤 40(40%),峰值 160 后回撤 50(31.25%)
        let dd = equity_drawdown(&[100.0, -40.0, 100.0, -50.0]);
        assert!((dd - 0.4).abs() < 1e-9, "{dd}");
        assert_eq!(equity_drawdown(&[]), 0.0);
        assert_eq!(equity_drawdown(&[10.0, 20.0]), 0.0, "只涨不回撤");
    }

    #[test]
    fn strategy_fills_are_scoped_by_user_strategy_and_account() { /* 建库:策略、信号(strategy_id)、工单、成交各两套,断言只取到本用户本策略本账户的 */ }

    #[test]
    fn execution_loss_is_median_of_real_vs_paper_price() { /* 同一 signal 下实盘 10.05、模拟 10.00 的买入 → 0.005;卖出方向取反 */ }
}
```

`judge.rs` 追加 watchdog 测试:

```rust
    #[test]
    fn watchdog_flags_drawdown_winrate_and_streak() {
        let cfg = AdmissionCfg::default();
        let base = BacktestBaseline { avg_trade_return: 0.02, trade_return_sd: 0.01, max_drawdown: 0.20 };
        let ok = WatchdogStats { drawdown: 0.25, recent_win_rate: 0.5, recent_trades: 20, max_consecutive_losses: 3 };
        assert!(judge_watchdog(&ok, &base, 0.55, 4, &cfg).is_none());

        let deep = WatchdogStats { drawdown: 0.31, ..ok.clone() };
        assert!(judge_watchdog(&deep, &base, 0.55, 4, &cfg).unwrap().contains("回撤"));

        // σ = sqrt(0.55×0.45/20) ≈ 0.111;阈值 ≈ 0.55 − 2×0.111 = 0.328
        let cold = WatchdogStats { recent_win_rate: 0.30, ..ok.clone() };
        assert!(judge_watchdog(&cold, &base, 0.55, 4, &cfg).unwrap().contains("胜率"));
        let few = WatchdogStats { recent_trades: 4, recent_win_rate: 0.0, ..ok.clone() };
        assert!(judge_watchdog(&few, &base, 0.55, 4, &cfg).is_none(), "样本不足不判定");

        let streak = WatchdogStats { max_consecutive_losses: 7, ..ok.clone() };
        assert!(judge_watchdog(&streak, &base, 0.55, 4, &cfg).unwrap().contains("连亏"));
    }
```

- [ ] **Step 2–4: 失败 → 实现 → 通过**

`strategy_fills` SQL(按用户 + 策略 + 账户):

```sql
SELECT f.account, f.code, f.side, f.price, f.qty, f.fee, f.realized_pnl, f.filled_at, t.signal_id
FROM trade_fills f
JOIN trade_tickets t ON t.id = f.ticket_id
JOIN trade_signals s ON s.id = t.signal_id
WHERE f.user_id = ?1 AND s.user_id = ?1 AND s.strategy_id = ?2 AND f.account = ?3
  AND (?4 IS NULL OR f.filled_at >= ?4)
ORDER BY f.id
```

`paper_stats`:`days` = `since` 到 `now` 之间的工作日数(用 `chrono` 逐日计数,跳过周末);`trades` = 卖出成交数;`avg_trade_return` = `trade_returns` 均值;`max_drawdown` = `equity_drawdown(卖出 realized 序列)`。

`execution_loss`:取同 `signal_id` 下 real 与 paper 各自第一笔成交价,按方向算比值,收集后取中位数;无配对返回 `None`。

- [ ] **Step 5: Commit**

```bash
git add src/trade/admission src/trade/config.rs
git commit -m "feat(trade): 模拟盘/实盘成交统计、执行损耗与 watchdog 判定"
```

---

### Task 5: 成绩单与评估任务执行

**Files:**
- Create: `src/trade/admission/scorecard.rs`、`src/trade/admission/worker.rs`
- Modify: `src/trade/admission/mod.rs`
- Test: 两个新文件内 `mod tests`

**Interfaces:**

```rust
// scorecard.rs
pub struct StageMetrics { pub trades: usize, pub win_rate: f64, pub avg_trade_return: f64, pub max_drawdown: f64, pub realized_pnl: f64 } // Serialize, Default
pub struct Scorecard { pub strategy_id: i64, pub status: StrategyStatus, pub oos: Option<PoolMetrics>, pub paper: StageMetrics, pub real: StageMetrics, pub execution_loss: Option<f64> } // Serialize
pub fn stage_metrics(fills: &[FillRow]) -> StageMetrics;
pub fn scorecard(conn: &Connection, user_id: i64, strategy_id: i64) -> Result<Scorecard>;
// worker.rs
pub struct EvalOutcome { pub job_id: i64, pub kind: EvalKind, pub note: String }
pub fn run_job<F>(conn: &mut Connection, job: &EvalJob, cfg: &WalkForwardCfg, admission: &AdmissionCfg, now: NaiveDateTime, load: F) -> Result<EvalOutcome>
where F: FnMut(&str) -> Result<Vec<StockBar>>;
```

`run_job` 行为:
- `WalkForward`:读策略 → `run_pool`(进度写 `set_job_progress`)→ `judge_backtest` → `apply_backtest_verdict`;note 写明通过与否
- `PaperCheck`:仅对 `Paper` 状态;取该策略进入观察期的时间(`trade_strategy_events` 最近一次 to=paper)→ `paper_stats` → `judge_paper`(基线来自最新 `oos` eval,mover 传 `None`)→ 通过则 `Paper → Admitted`,否则仅记 note(不降级)
- `Watchdog`:仅对 `Admitted`;`stats` 取实盘成交 → `judge_watchdog` → `Some(reason)` 则 `Admitted → Suspended` 并把原因写入状态

- [ ] **Step 1: 写失败测试(要点)**

`scorecard.rs`:`stage_metrics` 的纯函数测试(笔数、胜率、均值、回撤、已实现盈亏);`scorecard` 的集成式测试(建库 → 造信号/工单/成交 → 断言四栏与执行损耗)。

`worker.rs`:
- `walk_forward_job_moves_strategy_to_paper`:注入锯齿行情,策略 `submit_for_backtest` 后入队并 `run_job`,断言状态变 `Paper`、`latest_eval("oos")` 有值、job note 含「通过」
- `walk_forward_job_records_failure_reasons`:注入横盘行情(不满足阈值)→ 状态 `Failed`,`status_reason` 非空
- `paper_check_promotes_when_thresholds_met`:构造满足观察期条件的模拟盘成交 → `Paper → Admitted`
- `watchdog_suspends_on_deep_drawdown`:构造实盘亏损序列 → `Admitted → Suspended`,原因含「回撤」

- [ ] **Step 2–4: 失败 → 实现 → 通过**

- [ ] **Step 5: Commit**

```bash
git add src/trade/admission
git commit -m "feat(trade): 四栏成绩单与评估任务执行(回测/观察期/watchdog)"
```

---

### Task 6: trade-eval 线程与调度接线

**Files:**
- Modify: `src/trade/daemon.rs`、`src/trade/config.rs`、`src/main.rs`
- Test: `src/trade/daemon.rs` 内 `mod tests`、`tests/trade_admission.rs`(新建)

**Interfaces:**

```rust
// config.rs
pub struct EvalCfg { pub enabled: bool, pub poll_secs: u64, pub stock_cache_dir: PathBuf, pub daily_check_hour: u32, pub daily_check_minute: u32 } // 默认 true/30/".cache/stock"/15/40;TradeCfg 增加 pub eval: EvalCfg
// daemon.rs
pub fn spawn_eval(db_path: PathBuf, cfg: TradeCfg) -> std::io::Result<std::thread::JoinHandle<()>>;
pub fn enqueue_daily_checks(conn: &Connection, now: NaiveDateTime) -> Result<usize>;   // Paper → PaperCheck;Admitted → Watchdog
pub fn enqueue_monthly_backtests(conn: &Connection, now: NaiveDateTime) -> Result<usize>; // 每月首个工作日,Paper/Admitted → WalkForward
```

线程循环:心跳 `trade-eval` → `claim_next_job` → `run_job`(K 线用 `cache::load_or_fetch(code, &cfg.eval.stock_cache_dir, today-10年, today)`)→ `finish_job`;无任务时按 `poll_secs` 休眠;每个交易日 `daily_check_hour:minute` 调 `enqueue_daily_checks`,每月首个工作日调 `enqueue_monthly_backtests`;整轮 `catch_unwind`。

`main.rs`:在 Push 分支启动监听线程处一并 `spawn_eval`(受 `cfg.eval.enabled` 控制,失败只告警)。

集成测试 `tests/trade_admission.rs`:用内存库 + 注入的 K 线闭包,跑「入队 → run_job → 状态推进」的端到端链路(不起线程)。

- [ ] 各步骤照 Task 1–5 的模式:先写失败测试 → 实现 → 全量门禁 → Commit

```bash
git add src/trade src/main.rs tests/trade_admission.rs
git commit -m "feat(trade): trade-eval 评估线程、每日与每月调度接线"
```

---

-->

## 完成标准

- [ ] 基金与既有测试期望值未改动(`trade_stats` 重构后既有断言不变)
- [ ] `trade::admission`、`trade::store`、`stock::trade_stats` 的单元测试通过;测试不访问网络
- [ ] CI 门禁符合 Global Constraints

## 后续计划(不在本计划范围)

- **计划 3c**:模拟盘 / 实盘成交统计与执行损耗、watchdog 判定、四栏成绩单、评估任务执行与 `trade-eval` 线程(草案见本文件注释块)
- **计划 3d**:日线策略信号(收盘后计算、次日 09:25 发出、`admission_for` 接入 `submit_signal`)、`stock/recommend.rs` 迁移到 A 股口径、交易日历(节假日)
- **计划 4**:网页策略管理与成绩单展示、`/trade` 确认页、持仓校准、风控设置、按用户隔离的工单读取
- 计划 3a 遗留中未在本计划处理的:夏普衰减逐只中位数、`expand_grid` 重复展开与训练窗 clone、`aggregate` 默认 requested 的口径说明、`oos_annualized` 未参与判定
