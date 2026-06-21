# RSI 超买超卖短线策略 —— 设计文档

- 日期：2026-06-20
- 状态：已定稿（用户逐步确认通过）
- 依附：xlh 回测引擎 + config + Web 界面（已合入 master）

## 1. 目标

新增第四种策略 `rsi`：基于 N 日 RSI 的短线均值回归——RSI 跌破超卖线买入、升破超买线清仓。端到端接入引擎、config（CLI）、Web 界面（单次/对比/寻优三 Tab）。

非目标（YAGNI）：Wilder 平滑、RSI 背离、分批建仓、内建止盈止损（已有独立 rules 层可叠加在任意策略上）。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 逻辑 | RSI 超买超卖（均值回归） | 用户选定；与现有趋势/定投互补 |
| RSI 算法 | 简单平均法（最近 N 根日涨跌的算术平均） | 最简、可测；Wilder 列为非目标 |
| 触发 | 边沿触发（穿越超卖/超买线才动作） | 用户确认；避免连续重复下单，与 trend 同风格 |
| 买卖量 | 买入固定金额 `amount`、卖出全清 `AllOut` | 用户确认；与 trend 一致 |
| 参数 | `rsi_window` / `oversold` / `overbought` / `amount` | 标准 RSI 参数 |

## 3. 策略实现（新增 `src/strategy/rsi.rs`）

### 3.1 RSI 计算（模块内 helper）

```rust
/// 最近 window 根 bar 的 RSI（简单平均法），基于复权净值 adj_nav 的日涨跌。
/// 需要 history.len() >= window + 1（window 个涨跌）。不足返回 None。
/// 平均跌幅为 0（全涨）→ 100.0；平均涨幅为 0（全跌）→ 0.0。
fn rsi(history: &[MarketEvent], window: usize) -> Option<f64>;
```
实现：取 `history` 末尾 `window+1` 根的 `adj_nav`，逐对算 delta；`gain=max(delta,0)`、`loss=max(-delta,0)`；`avg_gain`/`avg_loss` 为各自均值；
- `avg_loss == 0`：返回 `100.0`（无下跌）。
- 否则 `rs = avg_gain / avg_loss`，返回 `100.0 - 100.0/(1.0+rs)`。
（`window==0` 或不足 `window+1` 根 → `None`。）

### 3.2 策略结构与行为

```rust
pub struct Rsi {
    window: usize,
    oversold: f64,
    overbought: f64,
    amount: f64,
    prev_rsi: Option<f64>,  // 上一根的 RSI，用于判断穿越
}
impl Rsi {
    pub fn new(window: usize, oversold: f64, overbought: f64, amount: f64) -> Self;
}
impl Strategy for Rsi {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent>;
}
```
`on_market` 逻辑：
1. `cur = rsi(ctx.history, self.window)`；为 `None` 则直接返回空（并保持 `prev_rsi` 不变——窗口未满不更新）。
2. 若有 `prev = self.prev_rsi`：
   - **买入**：`prev >= oversold && cur < oversold` → `SignalEvent{Buy, Cash(amount)}`。
   - **清仓**：`prev <= overbought && cur > overbought && ctx.shares > 1e-9` → `SignalEvent{Sell, AllOut}`。
   （两条件互斥，同一根至多一个信号。）
3. `self.prev_rsi = Some(cur)`，返回信号向量（0 或 1 个）。

依附既有：`crate::event::{Direction, SignalAmount, SignalEvent, MarketEvent}`、`crate::strategy::{Strategy, StrategyContext}`（与 trend.rs 相同 import）。

`src/strategy/mod.rs` 加 `pub mod rsi;`。

## 4. config 接线（src/config.rs）

- 新增参数结构：
```rust
#[derive(Debug, Deserialize)]
struct RsiParams { rsi_window: usize, oversold: f64, overbought: f64, amount: f64 }
```
- `build_strategy_from` 的 `match kind` 加分支：
```rust
"rsi" => {
    let p: RsiParams = params.try_into()?;
    if p.rsi_window < 1 { return Err(anyhow!("配置错误: rsi.rsi_window 必须 >= 1，当前值: {}", p.rsi_window)); }
    if !(0.0..=100.0).contains(&p.oversold) || !(0.0..=100.0).contains(&p.overbought) {
        return Err(anyhow!("配置错误: rsi.oversold/overbought 必须在 [0,100]"));
    }
    if p.oversold >= p.overbought {
        return Err(anyhow!("配置错误: rsi.oversold ({}) 必须小于 overbought ({})", p.oversold, p.overbought));
    }
    if p.amount <= 0.0 { return Err(anyhow!("配置错误: rsi.amount 必须 > 0，当前值: {}", p.amount)); }
    Box::new(Rsi::new(p.rsi_window, p.oversold, p.overbought, p.amount))
}
```
（在文件顶部 `use crate::strategy::rsi::Rsi;`。）

## 5. Web 接线

### 5.1 `src/web/mod.rs`
- `StrategyFields` 新增 3 个可选字段：`rsi_window: Option<usize>`、`oversold: Option<f64>`、`overbought: Option<f64>`（`amount` 已存在，复用）。
- `strategy_params_table` 加 `"rsi"` 分支：必填 `rsi_window`/`oversold`/`overbought`/`amount`，插入 toml 表（`rsi_window` 转 i64，其余 f64）。
- `build_optimize_cfg` 的 `keys` 加 `"rsi" => &["rsi_window","oversold","overbought","amount"]`。

### 5.2 `src/web/page.rs`（三处策略下拉 + 参数）
- 三个策略 `<select>`（单次 `.strat`、对比行 `strategySelect`、寻优 `#opt-strat`）各加 `<option value="rsi">RSI超买超卖</option>`。
- 单次：加一个 `data-for="rsi"` 参数组（RSI周期/超卖线/超买线/每次金额 → name=`rsi_window`/`oversold`/`overbought`/`amount`）。
- 对比行 `ROW_FIELDS.rsi = [["rsi_window","RSI周期","14"],["oversold","超卖线","30"],["overbought","超买线","70"],["amount","每次金额","1000"]]`。
- 寻优 `GRID_FIELDS.rsi = [["rsi_window","RSI周期(可多值)","14"],["oversold","超卖线(可多值)","25,30"],["overbought","超买线(可多值)","70,75"],["amount","每次金额","1000"]]`。
- 提交逻辑不变（JS 仍把字段并入 payload；`rsi_window` 等数值经 `Number()`）。

## 6. 测试

- **strategy（rsi.rs）**：
  - `rsi()` 数值：给一段已知涨跌序列断言 RSI 近似值；全涨序列→100；不足窗口→None。
  - 行为：构造价格先平稳后急跌（RSI 跌破 30）→ 断言出现 Buy；随后急涨（RSI 升破 70）且有持仓 → 断言出现 Sell(AllOut)。沿用 trend.rs 测试的 `run(prices,...)` 辅助风格。
- **config**：解析 `kind="rsi"` 的 TOML 成功构建；`oversold>=overbought` 报错（含 "oversold"）；`rsi_window=0` 报错。
- **web**：`build_strategy_from_fields` 对 rsi 字段全给 → ok、缺 `oversold` → 报错含 "oversold"；`build_optimize_cfg` rsi 网格（oversold/overbought 多值）→ grid 维度正确。
- **路由/前端**：`GET /` 含 `value="rsi"` 选项（断言 INDEX_HTML 含 `>RSI`）。
- **Playwright（扩展 verify_web.py，可选一档）**：单次 tab 选「RSI超买超卖」跑 161725 出报告，截图。
- 既有 89 测试保持绿、clippy 干净。

## 7. 单元边界

- `strategy::rsi`：`rsi()` 纯函数 + `Rsi` 状态机，独立可测，仅依赖 event/strategy 上下文。
- config / web 的 rsi 分支：装配，复用既有校验与参数表框架。
- page.rs：静态前端，加选项与字段。
- 经 `Box<dyn Strategy>` / toml 参数表 / `StrategyFields` 通信，与现有三策略对称。

## 8. 对既有的影响

- 纯新增：新策略文件 + 各处 `"rsi"` 分支 + 前端选项；不改 dca/smart_dca/trend、引擎、报告、指标。
- `StrategyFields` 加可选字段（serde `#[serde(default)]`），既有反序列化不破坏。
