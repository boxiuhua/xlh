# xlh

基金投资理财研判系统 —— A股基金定投/择时回测、参数寻优与市场状态诊断；并扩展支持**股票（A股/港股/美股）**的行情抓取、单股回测、技术诊断与跨股选股。

## 运行命令

```bash
cargo run --release -- serve          # 启动本地 Web 界面（默认 http://127.0.0.1:8080）
cargo run --release -- serve --port 9000
cargo run --release -- -c config.toml # 无子命令时按配置文件跑 CLI 回测/对比/寻优
```

---

## 数据存储

抓取的行情/净值以 **CSV 缓存在本地 `.cache/` 目录**（相对运行 `xlh` 的工作目录，如项目根 `./.cache/`）。基金与股票**分目录**存放；命中缓存且覆盖所请求日期区间即离线读取，否则重新抓取并覆盖写回，故第二次查同一标的为秒级。

| 类型 | 目录 | 文件名 | CSV 表头 |
|------|------|--------|----------|
| 基金净值 | `.cache/` | `{基金代码}.csv`（如 `161725.csv`） | `date,nav,acc_nav`（单位净值, 累计净值） |
| 基金清单 | `.cache/` | `fundlist.json` | 代码↔中文名映射（前端自动补全用） |
| 股票行情 | `.cache/stock/` | `{市场号}_{代码}.csv`（如 `1_600519.csv`） | `date,open,high,low,close,volume,adj_close`（`close` 不复权价, `adj_close` 后复权价） |

- **市场号**：`1`=沪、`0`=深、`116`=港股、`105/106/107`=美股（纳斯达克/纽交所/美交所）。股票用「市场号_代码」命名，避免三市场数字重码，也让基金「同步全部」不会误扫股票文件。
- **增量同步**：`/api/sync`（基金）、`/api/stock/sync`（股票）只把「晚于缓存最后一天」的新数据追加进已有 CSV。
- 数据源为**东方财富**（免费无 key）；缓存目录当前固定为 `.cache`（如需迁移到数据盘或改用数据库，可后续做成可配置）。

> 注：东财部分域名（如 `push2his.eastmoney.com`）为双栈解析，其 IPv6 CDN 节点在个别网络会返回空响应；股票抓取客户端已强制走 IPv4 规避。

---

## 技术架构

### 总览

xlh 是一个用 Rust 编写的**事件驱动回测引擎**，外层提供 CLI 与本地 Web（Axum）两套入口，共享同一套回测内核。整体分为「数据 → 策略 → 引擎撮合 → 组合记账 → 指标 → 报告」的单向数据流，辅以参数寻优与市场状态诊断两条旁路。

```
                 ┌────────── 入口层 ──────────┐
   CLI (main.rs) │                            │ Web (web/, Axum + Tokio)
                 └─────────────┬──────────────┘
                               ▼
        ┌──────────────── 回测内核 ────────────────┐
        │  data → strategy → engine → broker        │
        │                        → portfolio        │
        │                        → metrics          │
        └───────────────────────────────────────────┘
                               ▼
         report (html / chart / compare / optimize)
```

### 事件驱动管线

回测核心是一条四级事件流水线（`event.rs` / `engine.rs`）：

```
MarketEvent ──strategy──▶ SignalEvent ──portfolio──▶ OrderEvent ──broker──▶ FillEvent
   行情到达               策略产生买卖信号           风控转为确定订单         撮合扣费成交
```

`Engine`（`engine.rs`）每个交易日取一根 bar，压入事件队列，循环消费直至队列清空，再记录当日权益。各级职责清晰、互不耦合：

- **Market → Signal**：策略 `on_market` 基于「截至当日」的历史窗口生成信号；
- **Signal → Order**：`Portfolio::on_signal` 做基本风控（空仓不可卖、比例/现金额转份额），生成确定订单；
- **Order → Fill**：`Broker::execute` 按当日复权价撮合并扣费；
- **Fill**：`Portfolio::apply_fill` 记账，`Engine` 收集成交明细。

> **防偷看未来（look-ahead bias）**：`DataHandler::history` 只返回已发出的当日及之前的 bar，绝不暴露未来数据（见 `data/mod.rs` 的 `history_never_returns_future` 测试）。

### 模块分层

| 模块 | 职责 |
|------|------|
| `event.rs` | 四类事件（Market/Signal/Order/Fill）及 Direction、SignalAmount、OrderQty 等值类型 |
| `data/` | 数据层：`eastmoney` 抓取净值、`cache` CSV 本地缓存、`sync` 增量同步、`fundlist` 基金清单、`InMemoryData` 回放；并由单位净值+累计净值推导**复权净值**（隐含红利再投） |
| `strategy/` | 策略层：`Strategy` trait + 五种策略（`dca` 普通定投、`smart_dca` 智能定投、`trend` 均线择时、`rsi` 超买超卖、`adaptive` 自适应）；`RuleLayer` 以装饰器叠加止盈/止损 |
| `broker.rs` | 撮合与费用：FIFO 份额批次（lots）、买入费率、按持有天数分档的卖出阶梯费率 |
| `portfolio.rs` | 组合记账：现金、累计投入、权益曲线、XIRR 现金流（投入为负、期末市值为正） |
| `engine.rs` | 事件循环引擎，泛型于 `DataHandler` 与 `Strategy` |
| `metrics.rs` | 指标：总收益、最大回撤、夏普、XIRR（二分法求根的货币加权年化） |
| `analyze.rs` | 市场状态诊断（上升/下降/震荡）+ 均线±kσ 波动带的「高抛低吸」分档行动计划 |
| `optimize.rs` | 参数寻优：网格笛卡尔积展开 → 批量回测 → 按指标排序取 Top-N |
| `runner.rs` | 单次命名回测装配（data→engine→run→汇总） |
| `report/` | 报告：`html` 单次报告、`chart` 权益曲线图（plotters）、`compare` 多策略对比、`optimize` 寻优结果 |
| `config.rs` | TOML 配置解析与策略构建（`build_strategy_from`） |
| `stock/` | 股票体系（与基金业务代码互不 `use`，仅共用通用引擎）：`data/`（东财 K 线抓取/secid 三市场映射/后复权/CSV 缓存/搜索/同步 + `StockData` 引擎适配器）、`fee`（佣金+印花税+过户费）、`backtest`（单股回测）、`indicators`+`diagnose`（MA/MACD/布林/RSI 技术诊断）、`recommend`（跨股选股排名） |
| `web/` | Axum HTTP 服务 + 内嵌单页 HTML（`page.rs`）；组合根，同时接入基金与股票 |

### 关键设计

- **trait 抽象 + 泛型引擎**：`Engine<D: DataHandler, S: Strategy>` 对数据源与策略零成本泛型；`Box<dyn Strategy>` 也实现 `Strategy`，便于 `RuleLayer` 包裹与运行期组合。
- **策略装饰器**：`RuleLayer` 包裹任意内层策略，在其信号之上追加止盈（`TakeProfit`）/止损（`StopLoss`）清仓信号，正交于具体策略。
- **纯函数 + IO 分离**：Web 层把「校验+组装」（`build_run_from_query` 等纯函数，无 IO）与「加载数据+跑回测」分离；非 `Send` 的 `Box<dyn Strategy>` 在 `spawn_blocking` 线程内创建并消费，不跨 `await`。
- **缓存优先的数据获取**：`cache::load_or_fetch` 命中本地 CSV 则直接读，否则向天天基金抓取；`sync` 支持只追加「晚于缓存最后一天」的增量点。
- **安全**：基金代码白名单校验（拒绝路径穿越），HTML 输出统一转义。

### Web 接口

`web/mod.rs` 的 `router()` 暴露：

| 路由 | 方法 | 用途 |
|------|------|------|
| `/` | GET | 单页界面（基金：单次/对比/寻优/诊断/推荐；股票：股诊断/股回测/股选股） |
| `/api/run` | GET | 单次回测，返回 HTML 报告 |
| `/api/compare` | POST | 多策略对比 |
| `/api/optimize` | POST | 参数网格寻优 |
| `/api/regime` | GET | 市场状态诊断 + 高抛低吸行动计划（JSON） |
| `/api/funds` | GET | 基金清单（前端联想搜索） |
| `/api/sync` | POST | 净值数据增量同步 |
| `/api/stock/search` | GET | 股票代码/名称搜索（前端联想，跨三市场） |
| `/api/stock/diagnose` | GET | 单股技术诊断（趋势 + MA/MACD/布林/RSI 综合信号，JSON） |
| `/api/stock/run` | GET | 单股回测（费率按市场自动选，JSON 绩效+交易统计） |
| `/api/stock/recommend` | GET | 跨股选股：多策略样本外评分 + z-score 排名 Top-N |
| `/api/stock/sync` | POST | 股票行情增量同步 |

### 技术栈

Rust 2021 · axum 0.7 · tokio · reqwest（rustls）· plotters · clap · serde/serde_json · toml · chrono · anyhow/thiserror。
