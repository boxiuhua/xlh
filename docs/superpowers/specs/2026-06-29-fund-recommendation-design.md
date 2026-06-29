# 基金推荐页（Top5 + 策略 + 择时 + 依据）—— 设计文档

- 日期：2026-06-29
- 状态：已定稿（用户逐步确认通过）
- 依附：xlh 回测引擎 + Web 界面 + `analyze` 形态诊断（均已合入 master）

## 1. 目标

对一个**预设精选基金池**做跨基金分析，产出 **Top5 推荐**，每只给出：
**推荐策略 + 投资时间节点 + 推荐节奏 + 依据**。依据建立在**回测数据 + 样本外验证 + 形态匹配**之上，方法透明、可解释，并在页面上配有**算法说明**。

定位（与 `analyze` 一致）：基于历史净值的**统计回测 + 启发式规则**，**不预测涨跌、不构成投资建议**，免责声明常驻显眼处。

非目标（YAGNI）：全市场两万只逐只抓取、逐基金参数网格寻优、机器学习/外推预测、外接 LLM、自动下单。

## 2. 关键决策（用户已确认）

| 决策 | 选择 | 理由 |
|------|------|------|
| 基金范围 | 预设精选池（约 20–30 只主流代码） | 全市场联网逐只抓取不现实；精选池可控、首次同步后秒级复算 |
| Top5 口径 | 综合评分（多因子） | 纯历史收益易选出高波动赌徒品种；风险调整后「赚得多且稳」 |
| 前沿技术落地 | 现代量化方法（不预测） | 多因子风险调整评分 + 形态感知策略匹配 + 波动带择时 + 样本外验证 |
| 依据强度 | 加入样本外验证（walk-forward） | 训练段选策略、检验段实测，防过拟合，依据最扎实 |
| 策略参数 | 5 策略各用**固定稳健默认参数**（不逐基金寻优） | 减少过拟合、计算量可控、结果可复现 |
| UI | 新增第 5 个「推荐」Tab（含算法说明区块） | 与现有 4 Tab 一致；算法对用户透明 |

## 3. 精选池（`PRESET_POOL`，可增删）

代码常量内置，**中文名从已缓存的 `fundlist.json` 反查**（不硬编码中文名，避免与官方名不一致）。候选清单（混合宽基指数 / 行业 / 口碑主动，含现有 6 只缓存）：

```
161725 050002 000834 001427 003095 008888   // 现有缓存
110011 005827 110022 161005 163406 260108
000961 001593 519674 320007 002001 001714
000478 270042 040046 519066 005669 001102
```

> 说明：清单仅为合理默认，**用户可在 spec / 常量中增删**。任何代码若 `fundlist.json` 查不到名字，则以代码本身作展示名；若净值抓取失败则跳过并计入 `skipped`。最终以代码能否解析净值为准。

## 4. 核心逻辑 `src/recommend.rs`（纯函数、可单测，不碰 IO/网络）

### 4.1 数据结构（serde 序列化给前端）

```rust
use crate::analyze::RegimeReport;

/// 单个候选策略在「训练段选优 / 检验段验证」下的表现。
#[derive(Debug, Clone, serde::Serialize)]
pub struct StrategyEval {
    pub kind: String,        // dca/smart_dca/trend/rsi/adaptive
    pub name: String,        // 中文名
    pub is_return: f64,      // 训练段（in-sample）总收益（小数）
    pub is_sharpe: f64,
    pub is_mdd: f64,         // 训练段最大回撤（正数）
    pub oos_return: f64,     // 检验段（out-of-sample）总收益
    pub oos_sharpe: f64,
    pub oos_mdd: f64,
    pub score: f64,          // 该基金内、跨策略标准化后的训练段综合评分
}

/// 一只基金的完整推荐。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FundRecommendation {
    pub code: String,
    pub name: String,
    pub fund_score: f64,            // 跨基金标准化后的综合评分（排名用）
    pub best_strategy: StrategyEval,
    pub all_strategies: Vec<StrategyEval>, // 全部候选，透明对比
    pub regime: RegimeReport,      // 当前形态 + 行动计划（择时点）
    pub cadence_hint: String,      // 投资节奏建议（按形态）
    pub rationale: String,         // 一段「依据」文案（引用训练/检验数据 + 形态）
}

/// 整页推荐报告。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecommendReport {
    pub generated: String,         // 生成日期 YYYY-MM-DD
    pub pool_size: usize,          // 池内代码总数
    pub analyzed: usize,           // 成功分析数
    pub skipped: Vec<String>,      // 跳过的代码（数据不足/抓取失败）
    pub top: Vec<FundRecommendation>, // Top N（默认 5）
    pub weights: ScoreWeights,     // 当前权重（页面算法说明展示）
    pub split_ratio: f64,          // 训练段占比（0.7）
    pub disclaimer: String,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ScoreWeights { pub w_return: f64, pub w_sharpe: f64, pub w_mdd: f64 }
impl Default for ScoreWeights { /* 0.4 / 0.4 / 0.2 */ }

pub struct RecommendParams {
    pub top_n: usize,              // 默认 5
    pub split_ratio: f64,          // 默认 0.70
    pub weights: ScoreWeights,
}
impl Default for RecommendParams { /* top_n:5, split_ratio:0.70, weights:Default */ }
```

### 4.2 候选策略与固定默认参数

| kind | 中文名 | 固定默认参数 |
|------|--------|--------------|
| dca | 普通定投 | period=monthly, day=1, base_amount=1000 |
| smart_dca | 智能定投 | + ma_window=250, k=1.0 |
| trend | 均线择时 | short_window=20, long_window=60, amount=1000 |
| rsi | RSI超买超卖 | rsi_window=14, oversold=30, overbought=70, amount=1000 |
| adaptive | 自适应 | period=monthly, day=1, base_amount=1000 |

费率：买入费率 0（评分聚焦策略本身），卖出沿用 `web::standard_sell_tiers()` 口径；初始现金 0（与定投回测一致）。

### 4.3 评分管线（每只基金）

输入：该基金净值 `points: Vec<NavPoint>`（已按日期升序）。

1. **样本外切分**：`cut = floor(len * split_ratio)`；`train = points[..cut]`、`test = points[cut..]`。
   数据不足（`train` 或 `test` 不足以让引擎/指标产生有效结果，阈值 `train>=120 && test>=30`）→ 该基金跳过（计入 `skipped`）。
2. **训练段回测**：5 策略各用固定默认参数，在 `train` 上跑 `runner::run_one` 得 `Summary`（total_return / sharpe / max_drawdown）。
3. **训练段跨策略标准化打分**：对 5 个策略的三项指标各做 z-score（同一基金内），
   `score = w_r·z(return) + w_s·z(sharpe) − w_d·z(mdd)`。取 `score` 最高者为该基金**最优策略**。
   （组内 σ=0 时该项 z 记 0，避免除零。）
4. **检验段验证**：仅把**选中的最优策略**在 `test` 上回测，记录 `oos_return/oos_sharpe/oos_mdd`，写入 `best_strategy`。
5. **当前择时点**：对**全段** `points` 调 `analyze::detect_regime_with_plan`（默认 `RegimeParams`/`PlanParams`），得 `RegimeReport`（形态 + 波动带低吸/高抛线 + 当前信号/动作 + 距触发线）。若数据不足以判形态 → 该基金仍可入选，但 `regime` 缺省提示「数据不足，暂不给择时点」（不致命；实现上 detect 失败时填一个降级 RegimeReport，`plan=None`）。
6. **节奏建议** `cadence_hint`（按形态）：
   - 上涨趋势 → 「顺势持有 / 坚持定投，勿过早下车」
   - 震荡 → 「按波动带分批：低吸线买、高抛线减」
   - 下跌趋势 → 「谨慎，仅 −2σ 小额试探或观望」
7. **依据** `rationale`：模板化拼接，引用训练/检验实测，如
   「训练段(占比70%)在 5 个策略中『智能定投』综合评分最高（收益X% 夏普Y 回撤Z%）；检验段(后30%)实测收益A% 夏普B 回撤C%，样本外仍占优。当前形态：震荡，建议高抛低吸。」

### 4.4 Top5 排名（跨基金）

- 每只基金取「**最优策略的检验段(OOS)三项指标**」作为基金画像。
- 跨所有成功分析基金，对 OOS 三项各做 z-score → `fund_score = w_r·z(oos_return)+w_s·z(oos_sharpe)−w_d·z(oos_mdd)`。
- 按 `fund_score` 降序取前 `top_n`（默认 5）。

> 用 OOS 指标排名（而非训练段），与「依据=样本外」一致，进一步抑制过拟合幸存者偏差。

### 4.5 纯函数签名

```rust
/// 对单只基金净值产出推荐（不含跨基金排名所需的标准化 fund_score）。
/// 数据不足返回 Err（调用方据此跳过）。
pub fn evaluate_fund(code: &str, name: &str, points: &[NavPoint], p: &RecommendParams)
    -> anyhow::Result<FundRecommendation>;

/// 对一批已评估基金做跨基金 z-score 标准化、写回 fund_score、降序取 top_n。
pub fn rank_top(mut recs: Vec<FundRecommendation>, p: &RecommendParams)
    -> Vec<FundRecommendation>;
```

`src/lib.rs` 增 `pub mod recommend;`。仅依赖 `data::NavPoint`、`runner`、`metrics`、`analyze`、`config`/`web` 的策略构建助手——不引入新依赖、不碰网络。

## 5. Web 接线

### 5.1 路由 `GET /api/recommend`（`src/web/mod.rs`）

```rust
#[derive(Debug, Deserialize)]
pub struct RecommendQuery { #[serde(default)] pub top_n: Option<usize> }
```

`recommend_handler(Query(q)) -> Result<Json<RecommendReport>, AppError>`，在 `spawn_blocking` 内：
1. `params = RecommendParams { top_n: q.top_n.unwrap_or(5), ..Default::default() }`。
2. 加载 `fundlist`（`funds_payload`）建 code→name 映射。
3. 遍历 `PRESET_POOL`：`cache::load_or_fetch(code, ".cache", start, end)`
   （`end = today`，`start = end − 8 年`，留足训练/检验与均线/波动带窗口）。
   - 加载失败 → push 到 `skipped`，`continue`。
   - 成功 → `evaluate_fund(...)`；Err（数据不足）→ `skipped`。
4. `rank_top(recs, &params)` → 取 top_n。
5. 组装 `RecommendReport { generated: today, pool_size, analyzed, skipped, top, weights, split_ratio, disclaimer }` → `Json`。
- `router()` 注册 `.route("/api/recommend", get(recommend_handler))`。
- **性能**：首次需联网抓取池内全部基金（数十只，约几十秒），`spawn_blocking` 不阻塞服务；命中缓存后秒级。前端给「首次需联网抓取，请稍候」提示。

### 5.2 前端「推荐」Tab（`src/web/page.rs`）

- tabs 增第 5 项 `data-tab="recommend"`「推荐」+ 对应 panel。
- panel 结构：
  1. **算法说明区块**（可折叠 `<details>`，默认展开一次后可收起）：通俗列出
     - 综合评分公式 `score = 0.4·z(收益) + 0.4·z(夏普) − 0.2·z(最大回撤)` 与权重含义；
     - 样本外 70/30：训练段选策略、检验段验证、排名用检验段指标；
     - 5 个候选策略与固定参数；
     - 形态/波动带择时口径（低吸线 MA−σ、高抛线 MA+σ 等）；
     - 免责声明。
  2. 「生成推荐」按钮 + 运行中禁用 + spinner 文案。
  3. 结果区 `#rec-result`：渲染 `analyzed/pool_size/skipped` 概览 + Top5 卡片。
- 每张卡片（全部经 `esc()`）：
  - 头：排名 #1…#5 · 基金名（代码） · 综合评分。
  - **推荐策略** 徽章 + 中文名。
  - 关键指标行：样本外 收益% · 夏普 · 最大回撤%（并附训练段对照小字）。
  - **当前择时**：形态标签（红涨/绿跌/灰震荡）· 低吸线/高抛线（累计与等价单位净值）· 当前信号+动作 · 距触发线。
  - **依据** 段落（`rationale`）。
  - 节奏建议（`cadence_hint`）。
- 顶部/底部常驻**免责声明**：「基于历史净值的统计回测与启发式规则，不预测未来走势，不构成任何投资建议。」

## 6. 错误处理

- 单只基金抓取失败/数据不足 → 跳过并在 `skipped` 列出，不阻断整页。
- 全池均失败（如离线首次）→ `top` 为空，`RecommendReport` 仍合法返回，前端提示「暂无可分析数据，请先同步精选池/检查网络」。
- 非法/异常 → `AppError` 400（中文）。
- 前端 fetch 失败 → catch 显示。

## 7. 测试

- 单元（`recommend`）：
  - 样本外切分边界：`split_ratio=0.7` 下 `train/test` 长度正确；数据不足 → `evaluate_fund` Err。
  - z-score 标准化：组内 σ=0 不除零、各项符号正确（回撤为负贡献）。
  - 合成序列：构造一只单调上涨基金 → 趋势/智能定投类胜出且 `best_strategy.oos_return>0`。
  - `rank_top`：构造多只已评估基金，断言按 `fund_score` 降序、长度=top_n。
  - 序列化：`RecommendReport` JSON 含关键键（`top/best_strategy/regime/rationale/weights/disclaimer`）。
- 路由：`GET /api/recommend`（hermetic：用临时空 `.cache` → `top` 空、HTTP 200、合法 JSON）。
- 前端（`page.rs` 断言）：含 `data-tab="recommend"`、`/api/recommend`、`id="rec-result"`、算法说明关键字（如「综合评分」「样本外」）、「不构成」。
- Playwright（扩展 `scripts/verify_web.py`）：点「推荐」→「生成推荐」，断言出现 Top 卡片 + 「推荐策略」+ 算法说明 + 免责声明，截图 `output/web_recommend.png`。
- 既有测试保持绿、`clippy` 干净。

## 8. 单元边界

- `recommend::evaluate_fund` / `rank_top`：纯函数（净值+参数→推荐/排名），不碰 IO/网络/引擎内部，独立可测。
- `recommend_handler`：编排（参数+遍历池+load_or_fetch+evaluate+rank），薄。
- `page.rs` 推荐 Tab：静态前端 + fetch + 渲染。
- 经 `RecommendReport` JSON 通信。

## 9. 对既有的影响

纯新增：`recommend` 模块、一条只读 `GET /api/recommend`、前端第 5 Tab；不改引擎/策略/回测/既有 4 Tab。复用 `runner::run_one`、`metrics::Summary`、`analyze::detect_regime_with_plan`、`cache::load_or_fetch`、`fundlist`、web 的 `standard_sell_tiers`/`build_strategy_from_fields`/`validate_fund_code`/`AppError`/`esc`/`funds_payload`。

## 10. 免责与定位（重要）

推荐是**策略类型 + 历史回测画像 + 当前形态择时**的启发式组合，非买卖时点承诺、非收益保证。所有「依据」均为历史区间的回测统计；样本外验证仅降低过拟合风险，不代表未来表现。免责声明在 UI 常驻显眼处，算法说明区块同步呈现口径，确保透明可解释。
