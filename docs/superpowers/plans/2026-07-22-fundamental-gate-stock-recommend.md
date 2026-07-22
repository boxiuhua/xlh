# 基本面闸门前置到技术选股 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 `/api/stock/recommend` 技术选股在排名前加一道基本面闸门（复用 `screen::evaluate`），排除亏损/壳股/小市值/历史不足的 A 股，并把每只的 `Profile` 作为上下文附上；港美股不过闸只标注；不产生任何合成买入分。

**Architecture:** 纯逻辑 `recommend::build_report` 新增一个注入的 `gate` 闭包，在现有 `load`→`evaluate_stock`→`rank_top` 技术流程**之前**对每只股票判定过闸/排除/不适用。技术评分与排名逻辑一行不改。Web 层 `recommend_blocking` 组装 gate 闭包（A 股取全集快照+财报+估值调 `screen::evaluate`，港美股直接 NotApplicable）。前端 `sCard` 展示基本面上下文与不适用灰标，列表头展示闸门排除摘要。

**Tech Stack:** Rust 2021 · serde · anyhow · axum。测试用 `cargo test`。

## Global Constraints

- **铁律：不产生"基本面+技术"的合成买入分。** 排名数字仍只有现有技术 `stock_score`；`Profile` 仅作独立上下文字段，不得折算进任何分数。
- 复用 `screen::evaluate` / `screen::Exclusion` / `screen::Profile`，**不重写、不修改** `screen.rs` 的排除规则与其独立端点。
- `evaluate_stock` / `rank_top` / z-score / 技术评分逻辑保持不变。
- 非 A 股（港/美）原样放行，标 `GateStatus::NotApplicable`，不砍其选股能力。
- A 股基本面/估值抓取失败 ⇒ fail-open：保留该股并标 `NotApplicable("基本面数据获取失败,未经闸门")`，不误杀。
- 基本面展示措辞不得暗示买入（不出现"推荐/买入/看好/目标价"）。
- 所有对外报告保留现有 `DISCLAIMER`。

---

## File Structure

- `src/stock/recommend.rs`（修改）：新增 `GateStatus`、`GateOutcome`；`StockRecommendation` 加 `profile`/`gate`；`StockRecommendReport` 加 `gate_excluded`；`build_report` 加 `gate` 闭包与闸门循环。
- `src/web/stock.rs`（修改）：新增 `is_a_share` 辅助；重写 `recommend_blocking` 组装 gate 闭包。
- `src/web/page.rs`（修改）：`sCard` 加基本面上下文行/不适用灰标；`renderStockScreen` 头部加闸门排除摘要。

---

## Task 1: 后端 recommend.rs 闸门逻辑

**Files:**
- Modify: `src/stock/recommend.rs`
- Test: `src/stock/recommend.rs`（同文件 `#[cfg(test)]`）

**Interfaces:**
- Consumes: `crate::stock::screen::{Exclusion, Profile}`（`Profile: Serialize`；`Exclusion::reason() -> String`）。
- Produces:
  - `pub enum GateStatus { Passed, NotApplicable { reason: String } }`（内部标签 `kind` 序列化）。
  - `pub enum GateOutcome { Passed(Profile), Excluded(Exclusion), NotApplicable(String) }`。
  - `StockRecommendation` 新增公有字段 `profile: Option<Profile>`、`gate: GateStatus`。
  - `StockRecommendReport` 新增公有字段 `gate_excluded: Vec<(String, String)>`。
  - `build_report<F, G>(pool, names, today, p, load: F, gate: G)`，其中 `G: FnMut(&str) -> GateOutcome`。

- [ ] **Step 1: 加导入与两个枚举**

在 `src/stock/recommend.rs` 顶部 `use` 区（`use crate::stock::diagnose::...;` 之后）添加：

```rust
use crate::stock::screen::{Exclusion, Profile};
```

在 `ScoreWeights` 定义之前（约现 line 22）插入两个枚举：

```rust
/// 基本面闸门判定结果（随推荐项序列化给前端）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum GateStatus {
    /// A 股，已过基本面闸门。
    Passed,
    /// 港/美股或数据缺失，未经闸门 —— 附原因。
    NotApplicable { reason: String },
}

/// 闸门对单只股票的裁定（`build_report` 内部消费，不序列化）。
pub enum GateOutcome {
    /// 过闸，携带质量画像。
    Passed(Profile),
    /// 被排除（不进技术排名）。
    Excluded(Exclusion),
    /// 不适用（保留、标注、无画像）。
    NotApplicable(String),
}
```

- [ ] **Step 2: 给 StockRecommendation 加字段**

修改 `StockRecommendation`（现 line 42-49）为：

```rust
#[derive(Debug, Clone, Serialize)]
pub struct StockRecommendation {
    pub code: String, pub name: String, pub stock_score: f64,
    pub best_strategy: StockStrategyEval,
    pub all_strategies: Vec<StockStrategyEval>,
    pub diagnosis: StockDiagnosis,
    pub rationale: String,
    /// 基本面质量画像（A 股过闸者 Some）。**独立上下文，绝不折算进 stock_score。**
    pub profile: Option<Profile>,
    /// 基本面闸门状态。
    pub gate: GateStatus,
}
```

- [ ] **Step 3: 给 StockRecommendReport 加字段**

修改 `StockRecommendReport`（现 line 51-56），在 `skipped` 之后加 `gate_excluded`：

```rust
#[derive(Debug, Clone, Serialize)]
pub struct StockRecommendReport {
    pub generated: String, pub pool_size: usize, pub analyzed: usize,
    pub skipped: Vec<String>,
    /// 被基本面闸门在技术排名前踢掉的股票：(代码, 排除理由)。
    pub gate_excluded: Vec<(String, String)>,
    pub top: Vec<StockRecommendation>,
    pub weights: ScoreWeights, pub split_ratio: f64, pub disclaimer: String,
}
```

- [ ] **Step 4: evaluate_stock 构造体补默认字段**

在 `evaluate_stock` 末尾的 `Ok(StockRecommendation { ... })`（现 line 150-153）里，`rationale` 之后补两个占位字段（`build_report` 会覆盖它们；直接调用 `evaluate_stock` 时表示"无闸门信息"）：

```rust
    Ok(StockRecommendation {
        code: code.to_string(), name: name.to_string(), stock_score: 0.0,
        best_strategy: best, all_strategies: evals, diagnosis, rationale,
        profile: None,
        gate: GateStatus::Passed,
    })
```

- [ ] **Step 5: build_report 加 gate 闭包与闸门循环**

整体替换 `build_report`（现 line 172-204）为：

```rust
/// 遍历股票池：先过基本面闸门（gate），过闸/不适用者再注入 loader 取 bars → evaluate_stock
/// → rank_top，装配整页报告。被 gate 排除者不进技术排名。
pub fn build_report<F, G>(
    pool: &[&str], names: &HashMap<String, String>, today: &str, p: &RecommendParams,
    mut load: F, mut gate: G,
) -> StockRecommendReport
where
    F: FnMut(&str) -> anyhow::Result<Vec<StockBar>>,
    G: FnMut(&str) -> GateOutcome,
{
    let mut recs = Vec::new();
    let mut skipped = Vec::new();
    let mut gate_excluded: Vec<(String, String)> = Vec::new();
    for &code in pool {
        let (gate_status, profile) = match gate(code) {
            GateOutcome::Excluded(e) => { gate_excluded.push((code.to_string(), e.reason())); continue; }
            GateOutcome::Passed(prof) => (GateStatus::Passed, Some(prof)),
            GateOutcome::NotApplicable(reason) => (GateStatus::NotApplicable { reason }, None),
        };
        match load(code) {
            Ok(bars) => {
                let name = names.get(code).cloned().unwrap_or_else(|| code.to_string());
                match evaluate_stock(code, &name, &bars, p) {
                    Ok(mut r) => { r.profile = profile; r.gate = gate_status; recs.push(r); }
                    Err(_) => skipped.push(code.to_string()),
                }
            }
            Err(_) => skipped.push(code.to_string()),
        }
    }
    let analyzed = recs.len();
    let top = rank_top(recs, p);
    StockRecommendReport {
        generated: today.to_string(),
        pool_size: pool.len(),
        analyzed,
        skipped,
        gate_excluded,
        top,
        weights: p.weights,
        split_ratio: p.split_ratio,
        disclaimer: DISCLAIMER.to_string(),
    }
}
```

- [ ] **Step 6: 更新已有测试的 build_report 调用（补 gate 闭包）+ 加测试辅助**

在 `#[cfg(test)] mod tests` 内，`series` 辅助之后新增两个辅助：

```rust
    fn load_two(code: &str) -> anyhow::Result<Vec<StockBar>> {
        match code {
            "600519" => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.5).collect::<Vec<_>>())),
            _ => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.2).collect::<Vec<_>>())),
        }
    }

    fn dummy_profile() -> Profile {
        Profile {
            code: "600519".into(), name: "茅台".into(), years: 10,
            roe_median: Some(30.0), roe_streak: 10, revenue_cagr: Some(20.0),
            profit_cagr: Some(20.0), gross_margin: Some(90.0),
            market_cap: 1.5e12, pe_ttm: Some(18.0), pe_percentile: Some(0.3),
            note: "测试画像".into(),
        }
    }
```

把现有 `build_report_ranks_and_skips` 的调用改为在最后补一个 gate 闭包（其余不变）：

```rust
        let rep = build_report(&["600519", "000001", "BADX"], &names, "2026-07-01", &p, |code| {
            match code {
                "600519" => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.5).collect::<Vec<_>>())),
                "000001" => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.2).collect::<Vec<_>>())),
                _ => Err(anyhow::anyhow!("加载失败")),
            }
        }, |_| GateOutcome::NotApplicable("测试".into()));
```

把 `build_report_empty_pool` 的调用改为：

```rust
        let rep = build_report(&[], &names, "2026-07-01", &RecommendParams::default(),
            |_| Ok(Vec::new()), |_| GateOutcome::NotApplicable("测试".into()));
```

把 `report_serializes_frontend_keys` 的调用改为补 gate 闭包，并在其断言的 key 列表里加入 `"gate_excluded"`、`"gate"`、`"profile"`：

```rust
        let rep = build_report(&["600519"], &names, "2026-07-01", &RecommendParams::default(), |_| {
            Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.4).collect::<Vec<_>>()))
        }, |_| GateOutcome::Passed(dummy_profile()));
        let j = serde_json::to_string(&rep).unwrap();
        for key in ["\"top\"", "\"best_strategy\"", "\"all_strategies\"", "\"diagnosis\"",
                    "\"rationale\"", "\"weights\"", "\"split_ratio\"", "\"disclaimer\"",
                    "\"stock_score\"", "\"skipped\"", "\"gate_excluded\"", "\"gate\"", "\"profile\""] {
            assert!(j.contains(key), "JSON 应含 {key}");
        }
```

- [ ] **Step 7: 写新失败测试（闸门行为 + 不变量）**

在 `mod tests` 末尾（最后一个 `}` 之前）追加：

```rust
    #[test]
    fn gate_excludes_before_technical_ranking() {
        let names: HashMap<String, String> = HashMap::new();
        let rep = build_report(&["600519", "LOSSY"], &names, "2026-07-01", &RecommendParams::default(),
            load_two,
            |code| if code == "LOSSY" { GateOutcome::Excluded(Exclusion::LossMaking) }
                   else { GateOutcome::Passed(dummy_profile()) });
        assert!(rep.top.iter().all(|r| r.code != "LOSSY"), "被排除者不进技术排名");
        assert!(rep.gate_excluded.iter().any(|(c, _)| c == "LOSSY"), "排除应记入 gate_excluded");
        assert!(rep.top.iter().any(|r| r.code == "600519" && r.profile.is_some()), "过闸者应带 Profile");
    }

    #[test]
    fn non_a_share_kept_and_marked_not_applicable() {
        let names: HashMap<String, String> = HashMap::new();
        let rep = build_report(&["AAPL"], &names, "2026-07-01", &RecommendParams::default(),
            load_two, |_| GateOutcome::NotApplicable("非A股".into()));
        assert_eq!(rep.top.len(), 1, "不适用者保留在推荐里");
        assert!(rep.top[0].profile.is_none());
        match &rep.top[0].gate {
            GateStatus::NotApplicable { reason } => assert!(reason.contains("非A股")),
            GateStatus::Passed => panic!("应为 NotApplicable"),
        }
    }

    #[test]
    fn profile_does_not_affect_ranking_score() {
        // 同样两只股票：一次带 Profile 过闸、一次 NotApplicable，stock_score 必须完全一致
        // —— 证明 Profile 是旁证、绝没被折算进排名分（铁律）。
        let names: HashMap<String, String> = HashMap::new();
        let with_prof = build_report(&["600519", "000001"], &names, "2026-07-01",
            &RecommendParams::default(), load_two, |_| GateOutcome::Passed(dummy_profile()));
        let without = build_report(&["600519", "000001"], &names, "2026-07-01",
            &RecommendParams::default(), load_two, |_| GateOutcome::NotApplicable("非A股".into()));
        assert_eq!(with_prof.top.len(), without.top.len());
        for (a, b) in with_prof.top.iter().zip(without.top.iter()) {
            assert_eq!(a.code, b.code, "排名顺序不受 Profile 影响");
            assert!((a.stock_score - b.stock_score).abs() < 1e-12, "分数不受 Profile 影响");
        }
    }
```

- [ ] **Step 8: 运行测试确认失败**

Run: `cargo test --lib stock::recommend`
Expected: 编译期先失败（若忘改任何一处旧调用），改全后新测试通过、旧测试通过。若此步先看到编译错误，按错误信息补全 Step 1-6。

- [ ] **Step 9: 运行测试确认全绿**

Run: `cargo test --lib stock::recommend`
Expected: PASS（含 `gate_excludes_before_technical_ranking`、`non_a_share_kept_and_marked_not_applicable`、`profile_does_not_affect_ranking_score` 及所有原有测试）。

- [ ] **Step 10: 提交**

```bash
git add src/stock/recommend.rs
git commit -m "feat(recommend): build_report 加基本面闸门(不改技术排名)

新增 GateStatus/GateOutcome;StockRecommendation 带 profile/gate;
report 带 gate_excluded。过闸复用 screen::evaluate;不变量测试锁死
'Profile 不折算进 stock_score'。

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Web 层 recommend_blocking 组装闸门

**Files:**
- Modify: `src/web/stock.rs`（`recommend_blocking` 现 line 118-125；新增 `is_a_share` 辅助）
- Test: `src/web/stock.rs`（`#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: `recommend::{GateOutcome, build_report}`、`screen::{self, ScreenParams}`、`universe::{Listing, latest_trade_date, load_or_fetch}`、`fundamentals::load_or_fetch`、`valuation::load_or_fetch`、`crate::stock::data::secid::{resolve_offline, Resolved}`。
- Produces: `fn is_a_share(code: &str) -> bool`；重写后的 `recommend_blocking`。

- [ ] **Step 1: 写 is_a_share 的失败测试**

在 `src/web/stock.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
    #[test]
    fn a_share_classification() {
        assert!(is_a_share("600519"), "沪 A");
        assert!(is_a_share("000858"), "深 A");
        assert!(!is_a_share("00700"), "港股非 A");
        assert!(!is_a_share("AAPL"), "美股非 A");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::stock::tests::a_share_classification`
Expected: FAIL（`is_a_share` 未定义，编译错误）。

- [ ] **Step 3: 实现 is_a_share**

在 `recommend_blocking` 之前（现 line 117 附近）插入：

```rust
/// 是否沪深 A 股（market 0/1）。离线判定，不触发美股搜索网络请求。
fn is_a_share(code: &str) -> bool {
    use crate::stock::data::secid::{resolve_offline, Resolved};
    matches!(resolve_offline(code), Ok(Resolved::Ready(s)) if s.market == 0 || s.market == 1)
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib web::stock::tests::a_share_classification`
Expected: PASS。

- [ ] **Step 5: 重写 recommend_blocking 组装 gate 闭包**

整体替换 `recommend_blocking`（现 line 118-125）为：

```rust
fn recommend_blocking(q: RecommendQuery) -> StockRecommendReport {
    let params = RecommendParams { top_n: q.top_n.unwrap_or(5), ..Default::default() };
    let names = std::collections::HashMap::new();
    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days(8 * 365);

    // 基本面闸门数据：A 股全集快照（含市值/PE/名称）。加载失败则 A 股一律降级 NotApplicable，
    // 不阻断整轮选股。
    let trade_date = universe::latest_trade_date().unwrap_or(end);
    let snapshot: std::collections::HashMap<String, universe::Listing> =
        universe::load_or_fetch(universe_cache(), trade_date)
            .unwrap_or_default()
            .into_iter()
            .map(|l| (l.code.clone(), l))
            .collect();

    let gate = |code: &str| -> recommend::GateOutcome {
        if !is_a_share(code) {
            return recommend::GateOutcome::NotApplicable("非A股".into());
        }
        let Some(listing) = snapshot.get(code) else {
            return recommend::GateOutcome::NotApplicable("全集快照无此股".into());
        };
        let reports = match fundamentals::load_or_fetch(
            code, fundamentals_cache(), FUNDAMENTALS_MAX_AGE, end) {
            Ok(r) => r,
            Err(_) => return recommend::GateOutcome::NotApplicable("基本面数据获取失败,未经闸门".into()),
        };
        // 港股无估值历史；A 股正常应有，取不到则空序列（PE 分位因子自动降级）。
        let vals = valuation::load_or_fetch(code, valuation_cache(), trade_date).unwrap_or_default();
        match screen::evaluate(listing, &reports, &vals, &ScreenParams::default()) {
            Ok(profile) => recommend::GateOutcome::Passed(profile),
            Err(excl) => recommend::GateOutcome::Excluded(excl),
        }
    };

    recommend::build_report(STOCK_POOL, &names, &end.to_string(), &params,
        |code| cache::load_or_fetch(code, stock_cache(), start, end),
        gate)
}
```

- [ ] **Step 6: 运行整库测试确认编译与通过**

Run: `cargo test --lib`
Expected: PASS（含 Task 1 的 recommend 测试与本任务的 `a_share_classification`）。

- [ ] **Step 7: 提交**

```bash
git add src/web/stock.rs
git commit -m "feat(web): recommend_blocking 组装基本面闸门

A 股取全集快照+财报+估值调 screen::evaluate 过闸;港美股/数据缺失
标 NotApplicable。新增离线 is_a_share 判定。

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: 前端 page.rs 展示闸门结果

**Files:**
- Modify: `src/web/page.rs`（`sCard` 现 line 1158-1168；`renderStockScreen` 现 line 1169-1177）
- Test: 无单测（内嵌 HTML/JS）；手动核验 + `cargo build` 确保二进制含新页面。

**Interfaces:**
- Consumes（来自后端 JSON）：每个推荐项 `r.profile`（`{roe_streak, profit_cagr, pe_percentile}` 等，可为 `null`）、`r.gate`（`{kind:"Passed"}` 或 `{kind:"NotApplicable", reason}`）；报告 `rep.gate_excluded`（`[[code, reason], ...]`）。

- [ ] **Step 1: sCard 加基本面上下文行与不适用灰标**

整体替换 `sCard`（现 line 1158-1168）为：

```javascript
function sCard(r, rank){
  var d = r.diagnosis || {}, b = r.best_strategy || {}, tc = trendColor(d.trend);
  var extra = '';
  if (r.profile){
    var pf = r.profile, bits = [];
    if (pf.roe_streak != null && pf.roe_streak > 0) bits.push('ROE 连续 '+pf.roe_streak+' 年≥15%');
    if (pf.profit_cagr != null && isFinite(pf.profit_cagr)) bits.push('净利5年CAGR '+pf.profit_cagr.toFixed(1)+'%');
    if (pf.pe_percentile != null && isFinite(pf.pe_percentile)) bits.push('PE 自身历史分位 '+(pf.pe_percentile*100).toFixed(0)+'%');
    if (bits.length) extra = '<div style="margin-top:6px;color:#2c6e49">基本面（历史事实，非预测）：'+esc(bits.join(' · '))+'</div>';
  } else if (r.gate && r.gate.kind === 'NotApplicable'){
    extra = '<div style="margin-top:6px"><span style="background:#eef1f4;color:#7f8c8d;padding:1px 8px;border-radius:10px;font-size:.85rem">基本面闸门不适用：'+esc(r.gate.reason||'')+'</span></div>';
  }
  return '<div class="card" style="border-left:4px solid '+tc+'">'
    + '<div style="display:flex;align-items:baseline;gap:10px;flex-wrap:wrap"><span style="font-size:1.3rem;font-weight:700;color:#c0392b">#'+rank+'</span>'
    + '<span style="font-size:1.1rem;font-weight:600">'+esc(r.name)+'</span><span style="color:#7f8c8d">'+esc(r.code)+'</span>'
    + '<span style="margin-left:auto;color:#5a6a7a">综合评分 '+r.stock_score.toFixed(2)+'</span></div>'
    + '<div style="margin-top:8px">最优策略：<strong style="background:#fdecea;color:#c0392b;padding:2px 10px;border-radius:12px">'+esc(b.name)+'</strong></div>'
    + '<div style="margin-top:6px;color:#34495e">样本外：收益 '+pct(b.oos_return)+' · 夏普 '+b.oos_sharpe.toFixed(2)+' · 回撤 '+pct(b.oos_mdd)+'</div>'
    + '<div style="margin-top:6px;color:#34495e">技术面：<strong style="color:'+tc+'">'+esc(d.trend||'-')+'</strong> · '+esc(d.signal||'-')+'</div>'
    + extra
    + '<div style="margin-top:6px;color:#5a6a7a">'+esc(r.rationale)+'</div></div>';
}
```

- [ ] **Step 2: renderStockScreen 头部加闸门排除摘要**

整体替换 `renderStockScreen`（现 line 1169-1177）为：

```javascript
function renderStockScreen(rep){
  var box = document.getElementById('ss-result');
  if(!rep || !Array.isArray(rep.top)){ box.innerHTML = '<span style="color:#c0392b">选股失败</span>'; return; }
  if(!rep.top.length){ box.innerHTML = '<div style="color:#c0392b">暂无可分析数据（已分析 '+rep.analyzed+'/'+rep.pool_size+'）。</div>'; return; }
  var head = '<div style="color:#5a6a7a;margin-bottom:10px">已分析 '+rep.analyzed+'/'+rep.pool_size+' · 跳过 '+(rep.skipped||[]).length+' · 生成于 '+esc(rep.generated)+'</div>';
  var gx = rep.gate_excluded || [];
  if (gx.length){
    var agg = {};
    gx.forEach(function(pair){ var reason = pair[1] || '未知'; agg[reason] = (agg[reason]||0) + 1; });
    var parts = Object.keys(agg).map(function(k){ return esc(k)+' '+agg[k]+' 只'; });
    head += '<div style="margin-bottom:10px;color:#8a6d3b;background:#fcf8e3;border-radius:6px;padding:6px 10px">基本面闸门排除 '+gx.length+' 只：'+parts.join(' · ')+'</div>';
  }
  var cards = rep.top.map(function(r,i){ return sCard(r,i+1); }).join('');
  var foot = '<div class="hint" style="margin-top:6px;color:#c0392b">'+esc(rep.disclaimer)+'</div>';
  box.innerHTML = head + cards + foot;
}
```

- [ ] **Step 3: 构建确认页面编译进二进制**

Run: `cargo build`
Expected: 编译成功（页面为编译期内嵌字符串，编译通过即含新 JS）。

- [ ] **Step 4: 手动核验（可选，需联网）**

Run: `cargo run --release -- serve`，浏览器开 `http://127.0.0.1:8080` → "股" → "股选股" → "选股"。
Expected：A 股卡片出现绿色"基本面（历史事实，非预测）：…"行；港美股卡片出现灰标"基本面闸门不适用：非A股"；若有被排除项，列表上方出现黄底"基本面闸门排除 N 只：…"。无联网环境跳过此步。

- [ ] **Step 5: 提交**

```bash
git add src/web/page.rs
git commit -m "feat(web): 股选股卡片展示基本面上下文与闸门排除摘要

过闸 A 股显示 ROE 连续性/净利CAGR/PE 分位(标注'历史事实非预测');
港美股显示'闸门不适用'灰标;列表头汇总被排除只数与理由。

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage：**
- 前置闸门 + 复用 screen::evaluate → Task 1 Step 5 + Task 2 Step 5。✅
- 不产生合成分（铁律）→ Task 1 Step 7 `profile_does_not_affect_ranking_score` 不变量测试。✅
- 非 A 股 NotApplicable（方案 a）→ Task 1 Step 7 `non_a_share_kept...` + Task 2 gate 闭包。✅
- A 股 fail-open 标注 → Task 2 Step 5 `基本面数据获取失败,未经闸门`。✅
- gate_excluded 记录被排除项 → Task 1 Step 3/5 + Task 3 Step 2 摘要。✅
- 前端展示 Profile 上下文 + 不适用灰标 → Task 3 Step 1。✅
- 措辞不暗示买入 → Task 3 展示文案"历史事实，非预测"，不含买入词。✅
- evaluate_stock/rank_top 不变 → Task 1 仅新增字段与前置循环，未改这两函数体。✅

**2. Placeholder scan：** 无 TBD/TODO；每个代码步给出完整可编译代码与确切命令。✅

**3. Type consistency：** `GateStatus`/`GateOutcome`/`profile`/`gate`/`gate_excluded` 在 Task 1 定义，Task 2 以 `recommend::GateOutcome` 消费、Task 3 以 JSON `r.gate.kind`/`r.profile`/`rep.gate_excluded` 消费，命名一致。`Profile` 字段（`roe_streak`/`profit_cagr`/`pe_percentile`）与 `screen.rs::Profile` 定义一致。✅
