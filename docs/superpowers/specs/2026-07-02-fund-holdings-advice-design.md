# 基金「持仓建议」设计

日期：2026-07-02
状态：已批准，待实现

## 目标

在 Web「基金」大类下新增一个子 Tab「持仓建议」：用户录入自己的持仓（组合总额/持有收益/累计收益 + 每只基金的代码/金额/收益），系统按多策略样本外评估 + 当下择时，生成一份**逐只操作建议（加仓/持有/减仓/止盈/观望）+ 建议金额**的清单，并给组合汇总。

不预测未来、不构成投资建议——沿用现有免责声明。

## 复用现有能力

- `recommend::evaluate_fund(code, name, points, params) -> FundRecommendation`：已返回 `regime`（含 `plan: Option<ActionPlan>`，其 `current` 有 `z`/`signal`、波动带 `buy`/`sell` 线）、`best_strategy`、`all_strategies`（5 策略样本外）、`cadence_hint`、`rationale`。新功能在其上叠加持仓驱动的动作与金额。
- `data::cache::load_or_fetch(code, dir, start, end)`：加载/抓取净值（8 年窗口）。
- 前端：动态行、基金 combobox 补全、`renderPlan` 等样式。

## 数据结构（`src/holdings.rs`，纯逻辑，IO 注入）

```
Holding { code: String, amount: f64, profit: f64 }           // 单只持仓输入
HoldingsInput {
    total_amount: Option<f64>,      // 总持仓金额（选填，空则取各只 amount 之和）
    total_profit: Option<f64>,      // 持有收益（选填，回显）
    cumulative_profit: Option<f64>, // 累计收益（选填，回显）
    holdings: Vec<Holding>,
}

Action = 加仓 | 持有 | 减仓 | 止盈 | 观望   // 以字符串承载，附颜色由前端定

HoldingAdvice {
    code, name,
    amount, profit, weight,          // 权重 = amount / Σamount
    action: String,                  // 上述之一
    suggest_amount: f64,             // 建议金额（加/减方向由 action 表意，数值恒 ≥0）
    signal: String,                  // 当下择时信号（低吸/高抛/持有区间）
    z: f64,
    best_strategy: StrategyEval,     // 复用
    all_strategies: Vec<StrategyEval>,
    regime: RegimeReport,            // 复用（含 plan）
    rationale: String,               // 动作依据文案
}

PortfolioSummary {
    total_amount, total_profit, cumulative_profit,
    holding_count,
    total_add: f64,                  // 合计建议加仓额
    total_trim: f64,                 // 合计建议减仓/止盈额
    concentration_note: String,      // 集中度提示（最大权重 > 40% 时给出，否则空）
}

HoldingsReport {
    generated: String,
    summary: PortfolioSummary,
    advices: Vec<HoldingAdvice>,     // 与输入顺序一致
    skipped: Vec<String>,            // 加载失败/数据不足的代码
    disclaimer: String,
}
```

## 动作与金额逻辑

以 `z = plan.current.z`（波动带内位置，负=偏低偏便宜）与 `signal`（已含波动带+形态修正）为主，`regime.regime` 兜底：

- 无 plan / 数据不足 → 该只归入 `skipped`。
- 下跌趋势（`regime == "下跌趋势"`）→ **观望**；仅深跌（`z ≤ −1.5`）给**加仓**小额（按下方加仓公式但 pct 减半）。
- `signal` 含「低吸」→ **加仓**，`suggest = amount × clamp(0.10 + 0.10·min(|z|,2), 0.10, 0.30)`。
- `signal` 含「高抛」→ `profit > 0` 则 **止盈**，否则 **减仓**；`suggest = amount × clamp(0.10 + 0.10·min(z,2), 0.10, 0.30)`。
- 其余（持有区间）→ **持有**，`suggest = 0`。
- `amount <= 0` 的持仓：动作照常给出，`suggest = 0` 并在 rationale 注明「未填持有金额，仅给方向」。

金额四舍五入到整数元。

## 接口

`POST /api/holdings`，JSON body = `HoldingsInput`（字段名同上，`snake_case`）。
返回 `HoldingsReport`（JSON）。

Handler（web/mod.rs）：
- 8 年窗口，逐只 `cache::load_or_fetch(code, ".cache", start, end)`；失败或 `evaluate_fund` 报错 → `skipped`。
- code→中文名 用 `funds_payload` 映射（同 recommend）。
- `spawn_blocking` 包裹（联网/回测阻塞）。
- 空 holdings → 返回空 advices 的合法报告（前端提示先添加持仓）。

## 前端（page.rs，基金组新增子 Tab `holdings`）

- Tab 按钮加入 `#tabs-fund`：`<button class="tab" data-tab="holdings">持仓建议</button>`，面板 `#panel-holdings`。
- 表单：
  - 顶部行：总持仓金额 / 持有收益 / 累计收益（数字输入，选填）。
  - 动态持仓行容器 + 「+ 添加持仓」：每行 基金代码(combobox) + 持有金额 + 持有收益 + 删除。默认渲染 2 行。
  - 「生成建议」按钮。
- 渲染：
  - 组合汇总卡：回显总额/持有收益/累计收益、持仓数、合计加/减建议、集中度提示。
  - 逐只建议卡：动作徽章（加仓/止盈 红、减仓 绿、持有/观望 灰）+ 建议金额、择时信号+低吸/高抛线、最优策略及其样本外三指标、权重、rationale。
  - 底部免责声明。
- POST `/api/holdings`，请求/渲染错误按现有 `esc`/错误框风格处理。

## 测试

`holdings.rs` 单测（IO 注入、离线）：
- 低吸信号 → 加仓且 suggest>0；高抛+盈利 → 止盈；高抛+亏损 → 减仓；持有区间 → suggest=0。
- 下跌趋势 → 观望（非深跌 suggest=0）。
- 权重求和≈1；集中度提示在最大权重>40% 时出现。
- 加载失败/数据不足 → 计入 skipped，不 panic。
- 空持仓 → 合法空报告。
- 序列化含前端所需键。

web 层：`/api/holdings` 路由存在、坏 body/空持仓返回合法 JSON（沿用现有路由测试风格）。

## 范围外（YAGNI）

- 不做组合再平衡/目标配置优化（用户已明确选「逐只操作建议」）。
- 不做持仓持久化（每次前端提交，无账户存储）。
- 不接入实时行情，沿用日频净值缓存。
