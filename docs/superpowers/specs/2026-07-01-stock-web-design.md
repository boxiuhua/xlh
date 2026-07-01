# 股票 Web 层设计（子项目 5）

> 全局背景见 [子项目1 spec](2026-07-01-stock-data-layer-design.md)。本文档覆盖**子项目 5：Web 层**，把子项目1-4 的股票能力接入现有 Axum + 单页前端。前置：子项目1-4 已完成。

## 目标与范围

在现有 web 界面新增股票能力：数据来源自动补全、单股诊断、单股回测、跨股选股，全部经 `/api/stock/*` JSON 接口 + 前端新增 3 个 Tab。

**做**：`/api/stock/{search,diagnose,run,recommend,sync}` 路由 + 前端「股诊断 / 股回测 / 股选股」三 Tab + 股票代码服务端搜索补全。
**不做（YAGNI）**：股票 HTML 报告页（回测结果以 JSON 摘要卡片呈现，不做 iframe 报告）；图表；多语言。

## 设计要点

- **web 是组合根**：允许同时依赖基金与 stock 模块（隔离约束只限 `stock::*` 不 use 基金业务，web 不受限）。
- 股票 handler 单独放 `src/web/stock.rs` 子模块，保持 `web/mod.rs` 聚焦；路由在 `router()` 挂载。
- 复用现有 `build_strategy_from_fields`（共用策略工厂）构建策略；回测走子项目2 `stock::backtest::run_one`；诊断走子项目3；选股走子项目4。
- 股票缓存目录独立：`.cache/stock`（避免与基金 `.cache/*.csv` 混淆，基金 sync_all 不会误扫股票文件）。
- 费率按解析出的 secid 市场自动选 `StockFee::for_market`。
- 需为 `TradeStats`、`StockRunOutcome` 增加 `#[derive(Serialize)]`（供 JSON 返回）。

## 接口（`src/web/stock.rs`）

| 方法 | 路径 | 入参 | 出参 | 失败 |
|------|------|------|------|------|
| GET | `/api/stock/search` | `q` | `Json<Vec<StockInfo>>` | 无网/失败→空数组 |
| GET | `/api/stock/diagnose` | `code` | `Json<StockDiagnosis>` | 400(AppError) |
| GET | `/api/stock/run` | `code,start,end,strategy,策略参数...,initial_cash` | `Json<StockRunOutcome>` | 400 |
| GET | `/api/stock/recommend` | `top_n?` | `Json<StockRecommendReport>` | 400 |
| POST | `/api/stock/sync` | `{code?}` | `Json<Vec<SyncOutcome>>` | 恒 200(逐项 error) |

- `search`：`stock::data::search::search(q)`；异常降级空数组（对齐 `/api/funds`）。
- `diagnose`：解析 code→取近 ~800 日 bars→`diagnose::diagnose`（默认参数）。
- `run`：`validate_stock_code`→`resolve_secid` 取市场定 `StockFee::for_market`→`load_or_fetch`→`build_strategy_from_fields`→`backtest::run_one`。
- `recommend`：预设股票池 `STOCK_POOL`（A股/港股/美股混合，可增删）+ `.cache/stock` loader + `stock::recommend::build_report`。
- `sync`：`stock::data::sync::{sync_stock,sync_all}`，目录 `.cache/stock`。

`validate_stock_code`：非空、长度≤16、仅 `[A-Za-z0-9.]`（容纳 `600519`/`00700`/`AAPL`/`sh600519`/`us.AAPL`）。

## 前端（`src/web/page.rs`）

新增 3 个 Tab（`data-tab="s-diagnose"/"s-backtest"/"s-screen"`）与对应面板：
- **股诊断**：股票代码(服务端搜索补全) → 调 `/api/stock/diagnose` → 渲染趋势/信号/指标卡片。
- **股回测**：代码+日期+策略(复用策略下拉与参数组样式)+初始现金 → `/api/stock/run` → 渲染绩效+交易统计摘要卡片。
- **股选股**：Top-N → `/api/stock/recommend` → 渲染 TopN 卡片（评分/最优策略/样本外/技术面）。

新增 `attachStockCombobox(input)`：输入即查 `/api/stock/search?q=`，复用 `.fund-dropdown/.fund-item` 样式。免责声明沿用。

## 测试策略

- 路由存在性/契约（mirror 基金 web 测试，`tower::ServiceExt::oneshot`）：
  - `/api/stock/search?q=x` → 200 且 JSON 数组（无网降级空数组）。
  - `/api/stock/diagnose?code=bad!!` → 400。
  - `/api/stock/sync {code:"bad!!code"}` → 200、单元素带 error。
  - `/api/stock/recommend` 路由存在（不联网断言略；仅断言 `STOCK_POOL` 非空且代码合法）。
- 首页含三 Tab 标记与 `/api/stock/*` 端点、结果容器（HTML-contains）。
- `TradeStats`/`StockRunOutcome` 可 `serde_json` 序列化（单测）。

## 交付定义（Done）

- `cargo test` 全绿（新增 web/stock 测试 + 既有全部）。
- 首页含股诊断/股回测/股选股三 Tab，调用 `/api/stock/*`。
- 股票模块隔离不变；web 组合根接入股票能力。
