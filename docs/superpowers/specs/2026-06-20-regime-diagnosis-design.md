# 行情形态诊断 + 策略建议 —— 设计文档

- 日期：2026-06-20
- 状态：已定稿（用户逐步确认通过）
- 依附：xlh 回测引擎 + Web 界面（已合入 master）

## 1. 目标

对选定基金的近一段净值，**识别当前行情形态**（上涨趋势 / 下跌趋势 / 震荡），并据此**推荐适配的策略类型**（呼应「形态决定最优策略」的回测发现）。本质是基于历史净值的**描述性统计 + 启发式规则**，带醒目免责声明——**不预测涨跌、不构成投资建议**。

非目标（YAGNI）：真·净值预测、ML 模型、点位/择时预测、多基金排名推荐、自动用推荐策略回测。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 形态 | 三态：上涨趋势 / 下跌趋势 / 震荡 | 用户选定；透明可解释 |
| 信号源 | `acc_nav`（累计净值，含分红的总回报轨迹） | 反映真实趋势，避免分红除权造成的假跌 |
| 信号 | 区间收益 + MA20 vs MA60 + 年化波动率 | 经典、可解释、复用现成均线思路 |
| 窗口 | 默认 120 交易日（≈半年），可配 | 半年够看清当前形态、又不被远期噪声拖累 |
| 定位 | 描述性 + 启发式，强免责声明 | 历史净值无可靠预测力；诚实优先 |
| UI | 新增第 4 个「诊断」Tab | 与现有三 Tab 一致 |

## 3. 核心逻辑（新增 `src/analyze.rs`，纯函数可测）

```rust
use crate::data::NavPoint;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Regime { Uptrend, Downtrend, Range }

#[derive(Debug, serde::Serialize)]
pub struct RegimeReport {
    pub regime: String,          // 中文标签：上涨趋势/下跌趋势/震荡
    pub window: usize,           // 实际使用的交易日数
    pub window_return: f64,      // 区间收益（小数，如 0.12）
    pub annualized_vol: f64,     // 年化波动率（小数）
    pub ma_short: f64,
    pub ma_long: f64,
    pub ma_relation: String,     // 多头排列/空头排列/纠缠
    pub rec_strategy: String,    // 推荐策略 kind：smart_dca/trend/rsi
    pub rec_name: String,        // 推荐策略中文名
    pub rationale: String,       // 一句理由
}

pub struct RegimeParams {
    pub window: usize,           // 默认 120
    pub up_threshold: f64,       // 默认 0.10
    pub down_threshold: f64,     // 默认 -0.10
    pub ma_short: usize,         // 默认 20
    pub ma_long: usize,          // 默认 60
}
impl Default for RegimeParams { /* window:120, up:0.10, down:-0.10, ma_short:20, ma_long:60 */ }

/// 取 points 末尾 window 个点（基于 acc_nav）判定形态并给建议。
/// 数据不足（len < max(window, ma_long+1)）返回 Err("数据不足: ...")。
pub fn detect_regime(points: &[NavPoint], p: &RegimeParams) -> anyhow::Result<RegimeReport>;
```

判定步骤（全部基于 `acc_nav`）：
1. 数据量校验：`points.len() >= p.window` 且 `>= p.ma_long + 1`，否则 `Err`。
2. 取末尾 `window` 个点为窗口 `w`。
3. `window_return = w.last.acc_nav / w.first.acc_nav - 1`。
4. `ma_short = 末 ma_short 个 acc_nav 均值`；`ma_long = 末 ma_long 个 acc_nav 均值`（基于全 points 末尾，非窗口截断，保证 ma_long 有足量数据）。
5. `ma_relation`：`ma_short > ma_long*(1+ε)` → 多头排列；`< ma_long*(1-ε)` → 空头排列；否则 纠缠（ε=0.005 容差）。
6. `annualized_vol`：窗口内日收益（`acc_nav` 相邻比）标准差 × sqrt(252)。
7. **形态**：
   - `window_return > up_threshold` 且 ma_short > ma_long → `Uptrend`
   - `window_return < down_threshold` 且 ma_short < ma_long → `Downtrend`
   - 否则 → `Range`
8. **建议映射**：
   - Uptrend → `("smart_dca","智能定投","上涨趋势：顺势持有，频繁进出易踏空")`
   - Downtrend → `("trend","均线择时","下跌趋势：趋势走坏空仓离场，避开长跌")`
   - Range → `("rsi","RSI超买超卖","震荡：区间内高抛低吸吃波动")`

`src/lib.rs` 加 `pub mod analyze;`。复用 `data::NavPoint`（不依赖引擎/策略，避免循环）。

## 4. Web 接线

### 4.1 路由 `GET /api/regime`（src/web/mod.rs）
```rust
#[derive(Debug, Deserialize)]
pub struct RegimeQuery { pub fund_code: String, #[serde(default)] pub window: Option<usize> }
```
- handler `regime_handler(Query(q)) -> Result<Json<RegimeReport>, AppError>`：`spawn_blocking` 内——
  1. `validate_fund_code(&q.fund_code)?`（复用）。
  2. `end = chrono::Local::now().date_naive()`；`start = end - Duration::days(w*2 + 120)`（w=window 或默认120，留足历史给 MA60 与窗口）。
  3. `points = cache::load_or_fetch(&fund, ".cache", start, end)?`（与回测同款，缺失/陈旧自动抓）。
  4. `params = RegimeParams { window: q.window.unwrap_or(120), ..Default::default() }`。
  5. `detect_regime(&points, &params)?` → `Json`。
- 失败经 `AppError` → 400（数据不足/抓取失败/非法代码）。
- `router()` 注册 `.route("/api/regime", get(regime_handler))`。

### 4.2 前端「诊断」Tab（src/web/page.rs）
- tabs 加第 4 项 `data-tab="diagnose"` 「诊断」+ 对应 panel。
- panel：基金代码输入（带 `attachCombobox`）+ 窗口输入（默认 120）+ 「诊断」按钮 + 结果区 `#diag-result`。
- JS：`GET /api/regime?fund_code=&window=` → 渲染诊断卡：
  - 大字形态标签（上涨绿↑/下跌红↓/震荡灰——注意中国习惯红涨绿跌，这里**用颜色表态：上涨红、下跌绿、震荡灰**，与报告一致）。
  - 信号行：区间收益 X% · 年化波动 Y% · 均线 多头/空头/纠缠。
  - 推荐：**建议策略：智能定投**（中文名）+ 理由。
  - **免责声明**（小字常驻）：「基于历史净值的统计描述与启发式规则，不预测未来走势，不构成任何投资建议。」
  - 全部经 `esc()`；运行中禁用按钮。

## 5. 错误处理
- 数据不足（新基金/窗口过大）→ `detect_regime` Err → 400 中文提示。
- 非法 fund_code / 抓取失败 → AppError 400。
- 前端 fetch 失败 → catch 显示。

## 6. 测试
- 单元（`analyze`）：
  - 构造单调上涨序列（acc_nav 递增、区间收益 >10%）→ `Uptrend`、rec=smart_dca。
  - 单调下跌 → `Downtrend`、rec=trend。
  - 横盘小幅波动（收益≈0）→ `Range`、rec=rsi。
  - 数据不足（len < ma_long+1）→ Err 含「数据不足」。
  - 波动率：已知日收益序列断言 annualized_vol 数值合理（>0）。
- 路由：`GET /api/regime`（hermetic 用非法 code → 400；live 由 Playwright）。
- Playwright（扩展 verify_web.py）：诊断 tab 输入 161725 点诊断，断言出现形态标签 + 「建议策略」 + 免责声明，截图 `output/web_diagnose.png`。
- 既有 104 测试保持绿、clippy 干净。

## 7. 单元边界
- `analyze::detect_regime`：纯函数（净值+参数→报告），不碰 IO/网络/引擎，独立可测。
- `regime_handler`：编排（校验+日期+load_or_fetch+detect），薄。
- page.rs 诊断 tab：静态前端 + fetch。
- 经 `RegimeReport` JSON 通信。

## 8. 对既有的影响
- 纯新增：`analyze` 模块、一条只读 `GET /api/regime`、前端第 4 Tab；不改引擎/策略/回测/既有 Tab。
- 复用 `data::{NavPoint, cache::load_or_fetch}`、web `validate_fund_code`/`AppError`/`esc`/`attachCombobox`。

## 9. 免责与定位（重要）
该功能**不是预测**：它把当前净值形态归类，并映射到「历史上该类形态适配的策略类型」。免责声明在 UI 常驻显眼处。推荐是策略**类型**层面的启发式，非买卖时点、非收益承诺。
