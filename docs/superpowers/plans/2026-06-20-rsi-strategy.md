# RSI 超买超卖短线策略 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增第四种策略 `rsi`（RSI 超买超卖短线均值回归），端到端接入引擎、config(CLI)、Web 三 Tab。

**Architecture:** 新策略 `strategy::rsi::Rsi`（边沿触发：RSI 跌破超卖买入、升破超买清仓）；config 与 web 各加 `"rsi"` 分支（复用既有校验/参数表/网格框架）；page.rs 三处下拉加选项与参数。引擎/报告/指标不变。

**Tech Stack:** Rust（serde/toml）+ 现有 Web 界面 + Playwright。

## Global Constraints

- 纯新增：新策略文件 + 各处 `"rsi"` 分支 + 前端选项；不改 dca/smart_dca/trend、引擎、报告、指标。
- 不破坏既有 89 测试与 clippy 干净。
- RSI 简单平均法（最近 N 根日涨跌算术平均），非 Wilder；基于 `adj_nav`。
- 边沿触发：RSI 从 ≥oversold 下穿 <oversold → 买固定 `amount`；从 ≤overbought 上穿 >overbought 且有持仓 → 清仓 `AllOut`。
- 参数：`rsi_window`/`oversold`/`overbought`/`amount`；校验 `window≥1`、`0≤oversold<overbought≤100`、`amount>0`。
- `StrategyFields` 新字段用 `#[serde(default)] Option<...>`（不破坏既有反序列化）。
- edition 2021；提交含 `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` 尾注。

依附既有 API（已核对）：
- `event::{Direction, SignalAmount, SignalEvent, MarketEvent{date,nav,adj_nav}}`；`strategy::{Strategy, StrategyContext{today,history,shares,avg_cost,cash}}`。
- trend.rs 是同款参照（边沿触发 + Cash/AllOut + `#[cfg(test)]` 的 `run(prices,...)` 辅助）。
- `config::build_strategy_from(kind,&Option<toml::Value>,&[RuleCfg])`：`match kind` 有 dca/smart_dca/trend 分支，末 `other => return Err(anyhow!("未知策略: {other}"))`。
- web/mod.rs：`StrategyFields{strategy,period,day,base_amount,ma_window,k,short_window,long_window,amount}`；`strategy_params_table` match 有 dca/smart_dca/trend；`build_optimize_cfg` `keys` match；`run_blocking` 内策略友好名 match（mod.rs:284 区）。
- page.rs：单次 `<select name="strategy" class="strat">`、寻优 `<select id="opt-strat" class="strat-opt">`、对比 `strategySelect(cls)` 函数内联三 option；`ROW_FIELDS`/`GRID_FIELDS` 对象；单次参数组 `<span class="params" data-for="...">`。

---

## Task 1: 策略 strategy::rsi

**Files:**
- Create: `src/strategy/rsi.rs`
- Modify: `src/strategy/mod.rs`（加 `pub mod rsi;`）
- Test: `src/strategy/rsi.rs`（`#[cfg(test)]`）

**Interfaces:**
- Produces: `pub struct Rsi`；`pub fn Rsi::new(window: usize, oversold: f64, overbought: f64, amount: f64) -> Rsi`；`impl Strategy for Rsi`；模块内 `fn rsi(history: &[MarketEvent], window: usize) -> Option<f64>`。

- [ ] **Step 1: 注册模块**

`src/strategy/mod.rs` 在 `pub mod trend;` 附近加：

```rust
pub mod rsi;
```

- [ ] **Step 2: 写失败测试 + 占位**

创建 `src/strategy/rsi.rs`：

```rust
use crate::event::{Direction, SignalAmount, SignalEvent, MarketEvent};
use crate::strategy::{Strategy, StrategyContext};

/// 最近 window 根 bar 的 RSI（简单平均法），基于 adj_nav 日涨跌。
/// 需 history.len() >= window+1；不足返回 None。平均跌幅为 0 → 100.0。
fn rsi(history: &[MarketEvent], window: usize) -> Option<f64> {
    if window == 0 || history.len() < window + 1 { return None; }
    let slice = &history[history.len() - (window + 1)..];
    let mut gain = 0.0;
    let mut loss = 0.0;
    for w in slice.windows(2) {
        let d = w[1].adj_nav - w[0].adj_nav;
        if d >= 0.0 { gain += d; } else { loss += -d; }
    }
    let avg_gain = gain / window as f64;
    let avg_loss = loss / window as f64;
    if avg_loss == 0.0 { return Some(100.0); }
    let rs = avg_gain / avg_loss;
    Some(100.0 - 100.0 / (1.0 + rs))
}

/// RSI 超买超卖：RSI 下穿超卖线买入固定金额；上穿超买线清仓。
pub struct Rsi {
    window: usize,
    oversold: f64,
    overbought: f64,
    amount: f64,
    prev_rsi: Option<f64>,
}

impl Rsi {
    pub fn new(window: usize, oversold: f64, overbought: f64, amount: f64) -> Self {
        Self { window, oversold, overbought, amount, prev_rsi: None }
    }
}

impl Strategy for Rsi {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        let cur = match rsi(ctx.history, self.window) {
            Some(v) => v,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        if let Some(prev) = self.prev_rsi {
            if prev >= self.oversold && cur < self.oversold {
                out.push(SignalEvent { date: ctx.today.date, direction: Direction::Buy, amount: SignalAmount::Cash(self.amount) });
            } else if prev <= self.overbought && cur > self.overbought && ctx.shares > 1e-9 {
                out.push(SignalEvent { date: ctx.today.date, direction: Direction::Sell, amount: SignalAmount::AllOut });
            }
        }
        self.prev_rsi = Some(cur);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day.min(28)).unwrap() }

    fn bars(prices: &[f64]) -> Vec<MarketEvent> {
        prices.iter().enumerate()
            .map(|(i, p)| MarketEvent { date: d(2024, 1, (i + 1) as u32), nav: *p, adj_nav: *p })
            .collect()
    }

    #[test]
    fn rsi_values() {
        let b = bars(&[1.0, 2.0, 1.0]);
        assert!((rsi(&b, 2).unwrap() - 50.0).abs() < 1e-9, "涨1跌1 → RSI 50");
        let up = bars(&[1.0, 2.0, 3.0]);
        assert!((rsi(&up, 2).unwrap() - 100.0).abs() < 1e-9, "全涨 → RSI 100");
        let short = bars(&[1.0, 2.0]);
        assert!(rsi(&short, 2).is_none(), "不足 window+1 → None");
    }

    fn run(prices: &[f64], window: usize, oversold: f64, overbought: f64) -> Vec<SignalEvent> {
        let mut s = Rsi::new(window, oversold, overbought, 1000.0);
        let bs = bars(prices);
        let mut out = Vec::new();
        for i in 0..bs.len() {
            let ctx = StrategyContext { today: &bs[i], history: &bs[..=i], shares: if i > 0 { 100.0 } else { 0.0 }, avg_cost: 1.0, cash: 0.0 };
            out.extend(s.on_market(&ctx));
        }
        out
    }

    #[test]
    fn buys_on_oversold_cross_sells_on_overbought_cross() {
        // window=2, os=30, ob=70。价格 [3,3,3,2,1,4,5,1,5]：
        // i3 RSI 100→0 跌破30→Buy；i5 0→75 升破70→Sell(AllOut)；i7 100→20 跌破30→Buy
        let sigs = run(&[3.0, 3.0, 3.0, 2.0, 1.0, 4.0, 5.0, 1.0, 5.0], 2, 30.0, 70.0);
        assert!(sigs.iter().any(|s| s.direction == Direction::Buy), "应有买入");
        assert!(sigs.iter().any(|s| s.direction == Direction::Sell && s.amount == SignalAmount::AllOut), "应有清仓");
    }
}
```

把 `rsi` 函数体中的实现保留（上面已是完整实现，不是占位）。先确保测试存在。

- [ ] **Step 3: 运行确认（TDD 顺序：先只放测试会编译失败？）**

本任务实现与测试同文件给出。运行：
Run: `cargo test --lib strategy::rsi`
Expected: 3 测全过（`rsi_values`/`buys_on_oversold_cross_sells_on_overbought_cross`）。若失败按报错修正实现。

- [ ] **Step 4: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/strategy/mod.rs src/strategy/rsi.rs
git commit -m "feat: RSI 超买超卖短线策略"
```

---

## Task 2: config 接线 + CLI 冒烟

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: `strategy::rsi::Rsi`（Task 1）。
- Produces: `build_strategy_from` 支持 `kind="rsi"`。

- [ ] **Step 1: 写失败测试**

在 `src/config.rs` 的 `mod tests` 内新增：

```rust
    #[test]
    fn build_strategy_from_rsi_ok() {
        let params = toml::Value::Table({
            let mut t = toml::Table::new();
            t.insert("rsi_window".into(), toml::Value::Integer(14));
            t.insert("oversold".into(), toml::Value::Float(30.0));
            t.insert("overbought".into(), toml::Value::Float(70.0));
            t.insert("amount".into(), toml::Value::Float(1000.0));
            t
        });
        assert!(build_strategy_from("rsi", &Some(params), &[]).is_ok());
    }

    #[test]
    fn rejects_rsi_oversold_ge_overbought() {
        let params = toml::Value::Table({
            let mut t = toml::Table::new();
            t.insert("rsi_window".into(), toml::Value::Integer(14));
            t.insert("oversold".into(), toml::Value::Float(70.0));
            t.insert("overbought".into(), toml::Value::Float(30.0));
            t.insert("amount".into(), toml::Value::Float(1000.0));
            t
        });
        let err = build_strategy_from("rsi", &Some(params), &[]).unwrap_err();
        assert!(err.to_string().contains("oversold"), "应提示 oversold: {err}");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib config::tests::build_strategy_from_rsi_ok`
Expected: 失败（未知策略 rsi）。

- [ ] **Step 3: 实现 config 分支**

在 `src/config.rs` 顶部 `use` 区加：

```rust
use crate::strategy::rsi::Rsi;
```

新增参数结构（在其它 `*Params` 结构附近）：

```rust
#[derive(Debug, Deserialize)]
struct RsiParams { rsi_window: usize, oversold: f64, overbought: f64, amount: f64 }
```

在 `build_strategy_from` 的 `match kind` 中 `"trend" => {...}` 之后、`other => ...` 之前插入：

```rust
        "rsi" => {
            let p: RsiParams = params.try_into()?;
            if p.rsi_window < 1 {
                return Err(anyhow!("配置错误: rsi.rsi_window 必须 >= 1，当前值: {}", p.rsi_window));
            }
            if !(0.0..=100.0).contains(&p.oversold) || !(0.0..=100.0).contains(&p.overbought) {
                return Err(anyhow!("配置错误: rsi.oversold/overbought 必须在 [0,100]"));
            }
            if p.oversold >= p.overbought {
                return Err(anyhow!("配置错误: rsi.oversold ({}) 必须小于 overbought ({})", p.oversold, p.overbought));
            }
            if p.amount <= 0.0 {
                return Err(anyhow!("配置错误: rsi.amount 必须 > 0，当前值: {}", p.amount));
            }
            Box::new(Rsi::new(p.rsi_window, p.oversold, p.overbought, p.amount))
        }
```

（`build_strategy_from` 末尾把 base 包进 RuleLayer 的逻辑不变——rsi 与其它策略一样可叠加 rules。）

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib config`
Expected: 新两测 + 既有 config 测试全过。

- [ ] **Step 5: CLI 冒烟（真实数据，确认引擎+config 路径打通）**

创建临时配置并运行（白酒 161725 已缓存）：

```bash
cat > .tmp_rsi.toml <<'EOF'
[data]
fund_code="161725"
start="2020-01-01"
end="2024-12-31"
cache_dir=".cache"
[fees]
buy_rate=0.0015
sell_tiers=[{max_days=7,rate=0.015},{max_days=365,rate=0.005},{max_days=0,rate=0.0}]
[strategy]
kind="rsi"
[strategy.params]
rsi_window=14
oversold=30.0
overbought=70.0
amount=1000.0
[report]
chart=false
out_dir="output"
EOF
cargo run --quiet -- --config .tmp_rsi.toml 2>&1 | head -12
rm -f .tmp_rsi.toml
```
Expected: 打印「==== 回测结果 ====」与各指标（总收益/夏普等），无 panic。记录数值到报告。

- [ ] **Step 6: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/config.rs
git commit -m "feat: config 支持 rsi 策略 + 校验"
```

---

## Task 3: Web 后端接线

**Files:**
- Modify: `src/web/mod.rs`
- Test: `src/web/mod.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: Task 1/2 的 rsi 策略与 config 分支（`build_strategy_from` 经 `build_strategy_from_fields` 间接调用）。
- Produces: `StrategyFields` 含 rsi 字段；`strategy_params_table`/`build_optimize_cfg` 支持 rsi。

- [ ] **Step 1: 写失败测试**

在 `src/web/mod.rs` 的 `mod tests` 内新增（`sf` helper 已存在；为 rsi 单独构造）：

```rust
    fn sf_rsi() -> StrategyFields {
        StrategyFields {
            strategy: "rsi".into(),
            period: None, day: None, base_amount: None,
            ma_window: None, k: None, short_window: None, long_window: None,
            amount: Some(1000.0),
            rsi_window: Some(14), oversold: Some(30.0), overbought: Some(70.0),
        }
    }

    #[test]
    fn build_strategy_from_fields_rsi_ok() {
        assert!(build_strategy_from_fields(&sf_rsi()).is_ok());
    }

    #[test]
    fn build_strategy_from_fields_rsi_missing_oversold() {
        let mut f = sf_rsi();
        f.oversold = None;
        let err = build_strategy_from_fields(&f).err().expect("应返回 Err");
        assert!(err.to_string().contains("oversold"), "应提示缺 oversold: {err}");
    }

    #[test]
    fn build_optimize_cfg_rsi_grid() {
        let mut grid = std::collections::BTreeMap::new();
        grid.insert("rsi_window".into(), "14".into());
        grid.insert("oversold".into(), "25,30".into());
        grid.insert("overbought".into(), "70,75".into());
        grid.insert("amount".into(), "1000".into());
        let req = OptimizeRequest {
            fund_code: "161725".into(),
            start: NaiveDate::from_ymd_opt(2020,1,1).unwrap(),
            end: NaiveDate::from_ymd_opt(2024,12,31).unwrap(),
            buy_rate: 0.0015, initial_cash: 0.0,
            strategy: "rsi".into(), metric: "sharpe".into(), top_n: 5, grid,
        };
        let cfg = build_optimize_cfg(&req).unwrap();
        assert_eq!(cfg.grid.len(), 4);
        match cfg.grid.get("oversold").unwrap() {
            toml::Value::Array(a) => assert_eq!(a.len(), 2),
            _ => panic!("oversold 应为数组"),
        }
    }
```

注：现有 `sf(strategy)` helper 不含 rsi 字段，故新增 `sf_rsi()`。同时**既有 `sf()` helper 与 `base()`（RunQuery）构造体需补 3 个新字段**（见 Step 3 说明）以便编译。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::build_strategy_from_fields_rsi_ok`
Expected: 编译失败（`StrategyFields` 无 `rsi_window` 字段）。

- [ ] **Step 3: 实现**

(a) `StrategyFields` 末尾加 3 字段：

```rust
    #[serde(default)] pub rsi_window: Option<usize>,
    #[serde(default)] pub oversold: Option<f64>,
    #[serde(default)] pub overbought: Option<f64>,
```

(a2) **`RunQuery`（单次 GET 查询结构）末尾同样加 3 字段**——否则单次 tab 的 RSI 无法传参，且 `build_run_from_query` 里的 `StrategyFields` 字面量会缺字段编译失败：

```rust
    #[serde(default)] pub rsi_window: Option<usize>,
    #[serde(default)] pub oversold: Option<f64>,
    #[serde(default)] pub overbought: Option<f64>,
```

(a3) **更新 `build_run_from_query` 内 `let sf = StrategyFields {...}` 构造**，补映射这 3 个字段：

```rust
    let sf = StrategyFields {
        strategy: q.strategy.clone(),
        period: q.period.clone(), day: q.day, base_amount: q.base_amount,
        ma_window: q.ma_window, k: q.k,
        short_window: q.short_window, long_window: q.long_window, amount: q.amount,
        rsi_window: q.rsi_window, oversold: q.oversold, overbought: q.overbought,
    };
```

(b) `strategy_params_table` 的 `match s.strategy.as_str()` 中 `"trend" => {...}` 之后加：

```rust
        "rsi" => {
            need(s.rsi_window.is_some(), "rsi_window")?;
            need(s.oversold.is_some(), "oversold")?;
            need(s.overbought.is_some(), "overbought")?;
            need(s.amount.is_some(), "amount")?;
            t.insert("rsi_window".into(), (s.rsi_window.unwrap() as i64).into());
            t.insert("oversold".into(), s.oversold.unwrap().into());
            t.insert("overbought".into(), s.overbought.unwrap().into());
            t.insert("amount".into(), s.amount.unwrap().into());
        }
```

(c) `build_optimize_cfg` 的 `keys` match 加：

```rust
        "rsi" => &["rsi_window", "oversold", "overbought", "amount"],
```

(d) `run_blocking` 内策略友好名 match（`"trend" => "均线择时",` 那处）加：

```rust
        "rsi" => "RSI超买超卖",
```

(e) **编译修复**：`StrategyFields` 与 `RunQuery` 都加了 3 字段，既有测试 helper 的字面量构造需补：
- `sf(strategy: &str) -> StrategyFields` 字面量末尾补 `rsi_window: None, oversold: None, overbought: None`。
- `base(strategy: &str) -> RunQuery` 字面量末尾补 `rsi_window: None, oversold: None, overbought: None`。
（其它构造 `RunQuery`/`StrategyFields` 的测试若有，同样补这三行 `: None`。）

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: 新 3 测 + 既有 web 测试全过。

- [ ] **Step 5: clippy + 全量 + 提交**

Run: `cargo clippy --all-targets`（无 warning）然后 `cargo test`（全绿）
```bash
git add src/web/mod.rs
git commit -m "feat: web 后端支持 rsi（StrategyFields/参数表/寻优网格/友好名）"
```

---

## Task 4: 前端 page.rs 加 RSI

**Files:**
- Modify: `src/web/page.rs`
- Test: `src/web/mod.rs`（GET / 含 rsi 选项断言）

**Interfaces:**
- Consumes: Task 3 后端 rsi 字段。

- [ ] **Step 1: 写失败测试**

在 `src/web/mod.rs` 的 `mod tests` 内 `index_has_three_tabs` 附近加：

```rust
    #[tokio::test]
    async fn index_has_rsi_option() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("value=\"rsi\""), "策略下拉应含 rsi 选项");
        assert!(body.contains("RSI"), "应有 RSI 文案");
        assert!(body.contains("rsi_window"), "应有 rsi 参数字段");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::index_has_rsi_option`
Expected: 失败（现 INDEX_HTML 无 rsi）。

- [ ] **Step 3: page.rs 加选项与参数**

在 `src/web/page.rs` INDEX_HTML 中：

(a) 单次策略 `<select name="strategy" class="strat">` 内（`<option value="trend">均线择时</option>` 之后）加：

```html
            <option value="rsi">RSI超买超卖</option>
```

(b) 单次参数组：在 `data-for="trend"` 的 `<span class="params">` 之后加一个 rsi 参数组：

```html
        <span class="params" data-for="rsi" style="display:contents">
          <div class="field"><label>RSI周期</label><input type="number" name="rsi_window" value="14"/></div>
          <div class="field"><label>超卖线</label><input type="number" name="oversold" value="30"/></div>
          <div class="field"><label>超买线</label><input type="number" name="overbought" value="70"/></div>
          <div class="field"><label>每次金额</label><input type="number" name="amount" value="1000"/></div>
        </span>
```

(c) 寻优策略 `<select id="opt-strat" class="strat-opt">` 内（`trend` option 之后）加：

```html
            <option value="rsi">RSI超买超卖</option>
```

(d) 对比行的 `strategySelect(cls)` 函数返回串里，在 `trend` option 后加 rsi（保持单引号拼接风格）：

```javascript
  return '<select class="' + cls + '"><option value="dca">普通定投</option><option value="smart_dca" selected>智能定投</option><option value="trend">均线择时</option><option value="rsi">RSI超买超卖</option></select>';
```

(e) `ROW_FIELDS` 对象加 rsi：

```javascript
  rsi: [["rsi_window","RSI周期","14"],["oversold","超卖线","30"],["overbought","超买线","70"],["amount","每次金额","1000"]],
```

(f) `GRID_FIELDS` 对象加 rsi：

```javascript
  rsi: [["rsi_window","RSI周期(可多值)","14"],["oversold","超卖线(可多值)","25,30"],["overbought","超买线(可多值)","70,75"],["amount","每次金额","1000"]],
```

（提交逻辑不变：单次随策略显隐参数组的 `syncSingle` 已按 `data-for` 通用处理，rsi 组自动纳入；对比/寻优用 ROW_FIELDS/GRID_FIELDS 渲染。`oversold/overbought/rsi_window/amount` 在对比 JS 里走 `Number(v)`，period 才保留字符串——rsi 无 period，全数值，正确。）

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: `index_has_rsi_option` + 既有（含 `index_serves_form`/`index_has_three_tabs`/`index_has_fund_combobox`）全过。

- [ ] **Step 5: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/web/page.rs src/web/mod.rs
git commit -m "feat: Web 三 Tab 策略下拉加 RSI超买超卖 + 参数"
```

---

## Task 5: Playwright 端到端自测

**Files:**
- Modify: `scripts/verify_web.py`

- [ ] **Step 1: 扩展 verify_web.py 覆盖 RSI（单次 tab）**

在 `scripts/verify_web.py` 现有单次校验处之后、对比校验之前（或单次校验内），加一段：选单次策略为 rsi、运行、断言报告渲染。新增片段：

```python
        # ---- 单次 tab 选 RSI 策略 ----
        page.click('.tab[data-tab="single"]')
        page.select_option('#f-single select[name="strategy"]', "rsi")
        page.click("#run-single")
        rframe = page.frame_locator("#result")
        rframe.locator("canvas").first.wait_for(timeout=60000)
        rtext = rframe.locator("body").inner_text(timeout=10000)
        assert "总收益" in rtext, "RSI 报告应含 总收益"
        page.screenshot(path=str(Path("output/web_rsi.png").resolve()), full_page=True)
```

（保留脚本顶部清除 `*_PROXY` 逻辑与既有三 tab + 基金搜索校验。把最终 PASS 文案补充含 RSI，如 `print("PASS: 三 tab + 基金搜索 + RSI 策略 均正常")`。）

- [ ] **Step 2: 运行自测**

Run: `python scripts/verify_web.py`
Expected: 打印 PASS；生成 `output/web_rsi.png`。

- [ ] **Step 3: 肉眼核对截图**

Read `output/web_rsi.png`：单次 tab 策略为「RSI超买超卖」、参数为 RSI周期/超卖线/超买线/每次金额，iframe 内是收益曲线报告。异常按 systematic-debugging 修复后重跑。

- [ ] **Step 4: 全量测试 + 提交**

Run: `cargo test`（全绿）。
```bash
git add scripts/verify_web.py
git commit -m "test: Playwright 校验 RSI 策略单次回测"
```

交付报告：RSI 策略 CLI 与 UI 的实际回测数字、截图、各断言结果。

---

## Self-Review

- **Spec 覆盖**：§3 策略 rsi()+Rsi→T1；§4 config 分支+校验→T2；§5.1 web 后端（StrategyFields/params_table/optimize_cfg/友好名）→T3；§5.2 前端三处下拉+参数→T4；§6 测试→T1(策略)/T2(config+CLI)/T3(web)/T4(GET/)/T5(e2e)；§7/§8 边界与影响→各 Task 纯新增。
- **占位符**：无 TBD/TODO；每个改代码 Step 给完整代码。
- **类型一致**：`Rsi::new(window,oversold,overbought,amount)`(T1) = config(T2) 调用；`RsiParams{rsi_window,oversold,overbought,amount}`(T2) 字段名与策略参数表 toml key 一致；`StrategyFields` 新增 `rsi_window/oversold/overbought`(T3) = 前端 input name(T4) = strategy_params_table 取值(T3)；`build_optimize_cfg` keys "rsi"(T3) = GRID_FIELDS.rsi keys(T4)。
- **不破坏既有**：纯新增分支/字段/选项；StrategyFields 新字段 `#[serde(default)] Option`；既有策略与测试不动；T3 Step3(e) 提醒补 `sf()` helper 字面量字段以保编译。
- **Hermetic**：T1/T2/T3 单测无网络（构造数据/参数）；T2 CLI 冒烟用已缓存 161725；T5 e2e 用缓存。
- **YAGNI**：不做 Wilder、背离、分批、内建止盈止损。
