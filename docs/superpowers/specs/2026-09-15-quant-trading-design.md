# 量化交易(工单闭环 + 模拟盘 + 策略准入)—— 设计文档

日期:2026-09-15
状态:待实现(一期)

## 1. 背景与目标

系统已有策略(`src/strategy/`)、回测引擎(`src/engine.rs`)、实时行情与异动(`src/stock/realtime/`)、AI 分析(`src/ai.rs`)、推送(`src/push/channels.rs`),但**没有订单/成交/持仓闭环**:持仓靠手工录入(`src/holdings.rs`),信号推送后无法追踪是否执行、执行得如何、信号本身是否有效。

总体路线(三阶段,通过统一 `Broker` trait 演进,上层不改):

| 阶段 | 内容 |
|---|---|
| **一期(本文档)** | 交易工单闭环(人工线下成交 + 回填)、模拟盘、策略准入、止盈止损秒级监听 |
| 二期 | 多股票组合回测引擎(ETF 轮动/多因子)、交易 PIN |
| 三期 | 用户侧 `xlh-agent`(Python + miniQMT)主动连接服务器,确认即实盘下单 |

一期目标:

1. 四类信号源统一生成**交易工单**,经闸门(去重/冷却/准入/交易规则/风控)后推送
2. 用户在确认页确认,线下券商 App 成交后**回填**,系统更新持仓并持续监听止盈止损
3. 每个用户两个账户:`real`(人工回填)与 `paper`(模拟盘,自动成交),同一信号双轨对比
4. 策略必须通过**前推回测 + 模拟盘观察期**才能生成实盘工单,实盘期间持续监控、异常自动暂停
5. 成绩单量化「信号质量」与「人工执行损耗」

### 非目标(一期明确排除)

- 不接任何券商实盘接口(QMT/Ptrade),不做 GUI 自动化(easytrader 类方案永久排除)
- 不做多股票组合回测引擎
- 不做盘中实时计算日线策略信号
- 不做交易 PIN(预留开关)
- 服务器**永不保存**用户券商账号/密码

## 2. 现状约束(实现前必读)

| 约束 | 位置 | 影响 |
|---|---|---|
| 回测按当日收盘价成交 | `engine.rs:67` `broker.execute(&o, today.adj_nav)` | 与真实执行不符,需改为 T 日开盘价 |
| 无整手约束,可买小数股 | `broker.rs:102` | 小资金回测失真 |
| 无涨跌停/停牌/T+1/滑点 | 引擎 | 回测收益系统性虚高 |
| 引擎为**基金与股票共用** | `engine.rs`、`stock/backtest.rs` | 改动必须隔离,基金回测结果不得改变 |
| 寻优/推荐为单次 70/30 切分,检验段参与挑选 | `optimize.rs:151`、`page.rs:272` 自述 winner's curse | 准入改用 walk-forward |
| 引擎单标的 | `stock/backtest.rs:22` | 组合策略留二期 |
| 策略上下文只含 T-1 历史(无未来函数) | `strategy/mod.rs:28` | 保持该契约 |
| 调度为阻塞线程、不引入 tokio | `push/schedule.rs:64-77` | 新增后台工作使用 `std::thread` |
| 实时行情 10 分钟一次 | `stock/realtime/calendar.rs` | 止损需独立 15 秒小集合轮询 |
| 东财批量端点会封禁 | `2026-07-16-stock-realtime-design.md` | 监听只用腾讯快照 |
| 多用户 SaaS | `web/auth/` | 所有交易数据按 `user_id` 隔离 |

## 3. 架构

### 3.1 模块

```
src/trade/
├── mod.rs
├── model.rs          Signal / Ticket / Fill / Position / TicketStatus / Account(real|paper)
├── store.rs          建表、迁移、读写(data/xlh.db)
├── gate.rs           信号 → 工单:去重、冷却、准入、交易规则、风控、仓位计算
├── ticket.rs         工单状态机(条件更新保证幂等)
├── rules.rs          A 股交易规则:涨跌停比例、整手、T+1 可卖量
├── signal/
│   ├── exit.rs       止盈 / 止损 / 移动止盈
│   ├── strategy.rs   日线策略信号(复用 src/strategy)
│   ├── movers.rs     实时异动(接 realtime::job::run_tick 输出)
│   └── manual.rs     手动 / AI
├── broker/
│   ├── mod.rs        trait Broker { submit, cancel, positions }
│   ├── manual.rs     ManualBroker:确认后等待回填
│   └── paper.rs      PaperBroker:按 AShareExecution 撮合
├── quote.rs          trait QuoteSource(腾讯快照实现 + 测试桩)、trade_quotes 缓存
├── monitor.rs        trade-monitor 线程
├── admission/
│   ├── walk_forward.rs  前推回测
│   ├── state.rs         策略准入状态机
│   ├── watchdog.rs      持续监控
│   └── scorecard.rs     成绩单指标
├── eval_worker.rs    trade-eval 线程(任务队列)
└── link.rs           工单签名链接
```

引擎侧新增 `src/execution.rs`:`trait ExecutionModel`,实现 `FundExecution`(现有行为)与 `AShareExecution`。

### 3.2 数据流

```
 exit(15s) ─┐
 strategy ──┤                ┌─ 拒绝 → trade_signals(status=rejected, reason)
 movers ────┼─► Signal ─► gate
 manual ────┘                └─ 通过 ─┬─ real 工单(PENDING) ─► 推送 ─► /trade 确认页
                                      │                          │确认
                                      │                     CONFIRMED ─► 用户券商 App 下单
                                      │                          │回填
                                      │                     FILLED ─► trade_fills ─► trade_positions
                                      │                                                  │
                                      │                              exit 监听 ◄─────────┘
                                      └─ paper 工单 ─► PaperBroker(下一次快照价 + 滑点)
```

## 4. 数据模型

所有表含 `user_id`,位于 `data/xlh.db`,开启 WAL。

| 表 | 关键字段 |
|---|---|
| `trade_accounts` | user_id, kind(real/paper), total_capital, available_cash, updated_at |
| `trade_signals` | id, user_id, source(exit/strategy/mover/manual), strategy_id?, code, side, ref_price, reason, ai_note?, dedup_key(**唯一索引**), status(ticketed/rejected), reject_reason?, created_at |
| `trade_tickets` | id, user_id, signal_id, account(real/paper), code, side, suggest_price, qty, expires_at, deviation_th, status, urgency, confirmed_at, ignore_reason? |
| `trade_fills` | id, ticket_id, account, price, qty, fee, source(manual/paper/qmt), filled_at |
| `trade_positions` | user_id, account, code, qty, today_bought_qty, avg_cost, stop_loss, take_profit, trailing_pct?, trailing_high |
| `trade_position_adjusts` | 持仓校准记录:before/after/reason |
| `trade_risk_rules` | user_id, max_order_amount, max_position_pct, max_daily_tickets, daily_loss_halt_pct, cooldown_min, enabled |
| `trade_strategies` | id, user_id, kind, params(json), pool(json), version_hash, status, status_reason, updated_at |
| `trade_strategy_evals` | strategy_id, version_hash, stage(is/oos/paper/real), metrics(json), data_from, data_to, run_at |
| `trade_strategy_events` | strategy_id, from, to, reason, at |
| `trade_eval_jobs` | id, strategy_id, kind(walk_forward/paper_check/watchdog/monthly), status, error?, progress |
| `trade_quotes` | code, price, prev_close, limit_up, limit_down, suspended, ts |

全局开关:`[trade] enabled` 与管理员 kill switch(配置 + 管理后台)。

**持仓迁移**:用户首次启用交易模块时,将 `holdings.rs` 中的股票持仓导入 `trade_positions(account=real)`;此后旧持仓建议逻辑只读新表。

## 5. 信号源与工单

| 来源 | 触发 | 工单有效期 | 规则 |
|---|---|---|---|
| exit | trade-monitor 15s,触及止损/止盈/移动止盈 | 30 分钟;到期时价格仍在触发区 → 基于**同一 signal** 新建工单并 urgency+1(不新建信号) | dedup_key = position+rule+日期,同一规则当日只产生一个信号 |
| strategy | 15:30 收盘后用日线计算,次日 9:25 发出 | 次日 9:30–10:30 | 与回测口径一致(T-1 数据决策,T 开盘成交) |
| mover | 现有 `run_tick` 之后,仅用户自选股 | 10 分钟 | 已涨停不发买单;同码冷却 60 分钟;每日上限可配 |
| manual | 个股页 / AI 分析结果「生成工单」 | 用户自定,默认当日 | AI 内容自动填入理由 |

## 6. 闸门 `gate.rs`

按序执行,任一失败即拒绝并记录原因:

1. **去重**:同 user+code+side 存在未完结工单 → 拒绝;dedup_key 唯一索引兜底
2. **冷却**:冷却期内同 code+side → 拒绝
3. **准入**:strategy/mover 来源按策略状态 —— 已准入 → real + paper;观察期 → 仅 paper;其他 → 拒绝。**exit 与 manual 不受准入约束**
4. **交易规则**(`rules.rs`):涨停价不买、跌停价不卖;停牌不发;卖出 ≤ 可卖量(qty − today_bought_qty);买入整手取整(默认 100,`688` 最少 200)
5. **风控**:单笔金额上限;单票仓位上限;每日工单上限;当日亏损达 `daily_loss_halt_pct` 后仅允许卖出;用户/管理员总开关
6. **仓位计算**:买入股数 = floor(min(策略建议金额, 单笔上限, 仓位上限剩余额度) / 现价 / 手) × 手;为 0 → 拒绝(「资金不足一手」)

## 7. 工单状态机

```
PENDING ──确认──► CONFIRMED ──回填──► FILLED / PARTIAL
   │                 │
   ├─过期──► EXPIRED  └─撤销 / 次日 9:00 未回填──► CANCELLED
   └─忽略──► REJECTED
```

- 所有转换使用 `UPDATE … WHERE id=? AND status=?`,影响 0 行返回「已处理」(防重复点击与链接重放)
- 回填校验:数量 ≤ 工单数量;卖出 ≤ 可卖量;价格 > 0
- 回填后:写 fills → 更新 positions(均价、today_bought_qty)与 available_cash → 买入时设置默认止损/止盈(用户可改)

## 8. 确认页 `/trade`

- 标签:待确认 / 待成交 / 已完成 / 被拦截的信号 / 策略 / 风控设置 / 持仓校准
- 工单卡片:代码名称、方向、来源、剩余有效期、建议价、现价(读 `trade_quotes`,15s 刷新)与偏离、数量(可在风控内调整)、预估金额与费用、理由、策略成绩单、AI 说明
- **偏离保护**:|现价/建议价 − 1| > deviation_th(默认 1.5%)→ 按钮变为「价格已偏离,仍要确认」需二次点击
- **行情延迟**:`trade_quotes.ts` 超过 60s → 显示「行情延迟」并禁用确认
- 顶部显示 trade-monitor / trade-eval 心跳状态
- **签名链接**:推送中的 `/trade/t/{id}?sig=` 为 HMAC 签名(复用现有 hmac/sha2),仅允许查看与确认该工单,有效期至工单过期;预留 `require_pin` 开关(三期实盘时强制登录 + PIN)

## 9. 成交模型 `ExecutionModel`

```rust
trait ExecutionModel {
    /// 给定订单与当日 bar,返回成交(或 None 表示不可成交)
    fn fill(&mut self, order: &OrderEvent, bar: &Bar, prev_close_raw: Option<f64>) -> Option<FillEvent>;
}
```

- `FundExecution`:保持现行为(当日 adj_nav 成交),基金回测结果逐位不变
- `AShareExecution`(股票回测与模拟盘共用):

| 规则 | 实现 |
|---|---|
| 成交价 | T 日开盘价;复权开盘 = open × adj_close / close |
| 滑点 | 买 ×(1+s),卖 ×(1−s),默认 s=0.1%,可配 |
| 整手 | 买入向下取整到手(100;`688` 最少 200);卖出允许零股一次清 |
| 涨跌停 | 以**不复权**价判断:open ≥ 涨停价的买单、open ≤ 跌停价的卖单不成交,订单作废 |
| 比例 | 主板 10%、ST 5%、`300`/`688` 20%、北交所 30%;上市前 5 日跳过 |
| T+1 | 当日买入份额当日不可卖 |
| 停牌 | 当日无 bar 不成交 |
| 费用 | 沿用 `StockFee::a_share()` |

## 10. 策略准入

### 10.1 策略定义与版本

策略 = kind + params + pool,属于用户。params 或 pool 变更 → 新 version_hash → 状态重置为 DRAFT。

### 10.2 状态机

```
DRAFT ─提交─► BACKTESTING ─不达标/数据不足─► FAILED(原因)
                  │达标
                  ▼
               PAPER(仅模拟盘)─满期且达标─► ADMITTED(实盘 + 模拟盘)
                  ▲                              │
                  └────────── SUSPENDED ◄───────┘ watchdog 触发
                   (需用户手动重跑回测后重新提交)
```

mover 类策略:DRAFT 直接进入 PAPER(无历史分时数据,不可回测)。

### 10.3 前推回测(walk-forward)

- 训练窗 2 年、检验窗 6 个月、步长 6 个月
- 参数只在训练窗选取(复用 optimize 网格);检验窗仅运行、不参与选择
- 拼接所有检验窗得样本外曲线;池内逐只运行后汇总
- 数据 < 3 年 → FAILED(「数据不足,需要 ≥ 3 年」)

### 10.4 默认阈值(`config.toml [trade.admission]`,管理员可设,用户只可调严)

| 关卡 | 指标 | 默认 |
|---|---|---|
| 回测 | 样本外年化收益 | > 0 且 > 同期买入持有 |
| 回测 | 样本外夏普 | ≥ 0.8 |
| 回测 | 样本外最大回撤 | ≤ 25% |
| 回测 | 样本外交易笔数 | ≥ 30 |
| 回测 | 夏普衰减(1 − OOS/IS) | ≤ 50% |
| 回测 | 池内正收益占比 | ≥ 55% |
| 模拟盘 | 观察期 | ≥ 20 交易日 且 ≥ 10 笔(mover:40 日 / 30 笔) |
| 模拟盘 | 平均每笔收益 | ≥ 回测均值 − 1σ |
| 模拟盘 | 最大回撤 | ≤ 回测最大回撤 |

### 10.5 持续监控(每日收盘后 + 每月重跑)

任一触发 → SUSPENDED 并推送:

- 滚动回撤 > 1.5 × 回测最大回撤
- 近 20 笔胜率 < 回测胜率 − 2σ
- 连亏笔数 > 1.5 × 回测最长连亏
- 月度重跑前推回测不达标

### 10.6 成绩单

列:样本内 / 样本外 / 模拟盘 / 实盘。行:年化收益、超额、夏普、最大回撤、胜率、盈亏比、交易笔数、平均持仓天数。

附加 **执行损耗** = 同一信号 real 成交价相对 paper 成交价的**中位数**偏差(买入为正表示多付;中位数对个别极端成交更稳健)。
manual 来源归入「人工/AI」成绩单,不参与准入。

## 11. 运行时

`xlh push` 进程内新增两个 `std::thread`,各持独立 SQLite 连接,各自写心跳:

| 线程 | 频率 | 职责 |
|---|---|---|
| trade-monitor | 交易时段 15s | 拉取「持仓股 ∪ 未完结工单股」腾讯快照 → 写 trade_quotes → exit 信号 → paper 撮合 → 工单过期 |
| trade-eval | 队列驱动,空闲 30s 轮询 | walk_forward / paper_check / watchdog / monthly 任务 |

现有 60s 主循环新增:9:25 发策略工单、15:05 回填提醒、15:30 策略信号计算与 watchdog 入队、9:00 未回填工单撤销;mover 工单接在 `run_tick` 之后。

## 12. 异常处理

| 场景 | 处理 |
|---|---|
| 快照失败/超时 | 退避 15s→30s→60s;连续失败 ≥ 3 分钟推送「止损监听中断」给有持仓用户 |
| 数据过期/节假日 | 复用 `calendar::verify_fresh` / `stale_means_holiday`,非交易日不判止损、不处理过期 |
| 单票停牌/无报价 | 跳过;卡片标注「停牌」 |
| 进程重启 | 线程无状态;启动时批量过期超时工单;不补发错过的 exit,只按当前价重判 |
| 重复信号 | dedup_key 唯一索引,冲突跳过 |
| 重复确认/链接重放 | 条件更新,0 行 → 「已处理」 |
| 回填越界 | 前后端双重校验拒绝 |
| 忘记回填 | 15:05 提醒;次日 9:00 CANCELLED;提供持仓校准并留痕 |
| eval 任务失败/数据不足 | 策略 → FAILED 并写原因,不滞留 BACKTESTING |
| 推送失败 | 仅记日志,工单照常生成 |

## 13. 测试

- `AShareExecution`:开盘成交、滑点、整手(含 688)、一字涨停买不进、一字跌停卖不出、T+1、停牌;`FundExecution` 回归 —— 现有基金回测结果逐位不变
- `gate.rs`:表驱动,每条规则 通过/拒绝 + 断言原因
- 工单状态机:全部合法转换、非法转换拒绝、并发重复确认
- walk-forward:合成「训练窗好、检验窗失效」的过拟合策略 → 必须 FAILED;稳定策略 → 通过
- watchdog:构造回撤超标、连亏超标序列 → SUSPENDED
- exit 监听:`QuoteSource` 测试桩注入价格序列,断言触发时机与当日只触发一次
- 集成 `tests/trade_pipeline.rs`:信号 → 闸门 → 工单 → 确认 → 回填 → 持仓 → 止损触发 → 卖出工单
- CI:沿用 fmt / clippy / test

## 14. 一期任务拆分

| # | 任务 | 依赖 | 估时 |
|---|---|---|---|
| 1 | `ExecutionModel` + `AShareExecution` + 基金回归测试 | — | 3d |
| 2 | `trade/model.rs` + `store.rs` 建表迁移 + 持仓导入 | — | 2d |
| 3 | 工单状态机 + `Broker` trait + Manual/Paper | 1,2 | 2d |
| 4 | `gate.rs` + `rules.rs` | 2 | 2d |
| 5 | trade-monitor 线程(快照/exit/paper 撮合/过期/缓存/心跳) | 3,4 | 2d |
| 6 | 四类信号源接入 | 4,5 | 2d |
| 7 | walk-forward + 准入状态机 + trade-eval 队列 | 1,2 | 3d |
| 8 | watchdog + 成绩单(含执行损耗) | 3,7 | 2d |
| 9 | Web API + `/trade` 页面 + 签名链接 + 风控设置 + 持仓校准 | 3,4,7 | 3d |
| 10 | 推送文案、日报、集成测试、文档 | 全部 | 2d |

1 与 2 可并行;7 可与 4/5/6 并行。合计约 3–4 周。

## 15. 合规与风险提示

- 所有实盘工单须用户本人确认;系统不做无人值守下单。页面与协议写明「策略由用户自行配置、决策由用户自主作出」
- 三期接入 API 前,用户需经券商完成程序化交易报备(《证券市场程序化交易管理规定》)
- 向付费用户推送个股买卖信号可能涉及投资顾问资质边界,**上线实盘能力前需法律评估**
- 任何策略与准入机制均不保证盈利;准入的作用是淘汰无效策略、控制亏损

## 16. 后续阶段接口预留

- `Broker` trait 与 `trade_fills.source` 已预留 `qmt`
- 三期 `xlh-agent`:用户 Windows 机器上运行,凭 per-user token 通过 WebSocket 主动连接服务器,拉取 CONFIRMED 工单 → miniQMT 下单(工单号作为委托备注保证幂等)→ 回报成交与持仓;服务器不持有券商凭证
- `link.rs` 的 `require_pin` 开关在三期默认开启
