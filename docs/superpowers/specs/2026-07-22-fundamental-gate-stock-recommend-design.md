# 基本面闸门前置到技术选股（Feature A1）

日期：2026-07-22
状态：已批准，待实现

## 背景与动机

系统里其实已经有基本面数据链路，但和技术选股互不相通：

- `src/stock/recommend.rs` —— 技术/回测选股（`/api/stock/recommend`）。对每只股票在 5 个策略上做样本内/外回测，用样本外三指标（收益/夏普/回撤）跨股 z-score 排名。**只看行情，不看基本面。**
- `src/stock/screen.rs` —— 基本面筛选（`/api/stock/screen`）。用财报+估值做 ROE 持续性、CAGR、PE 自身历史分位、市值下限、亏损/壳股排除。**刻意只做"排除"、不给"综合评分"**（模块顶部注释记录了 103-agent 对抗式研究结论：百倍股事前特征阈值没通过验证，是幸存者偏差；测试里 `assert!(!json.contains("\"score\""))` 明确禁止总分字段，防止清单被读成"翻倍名单"）。

痛点：技术选股可能把亏损股/壳股排进"推荐"，用户在推荐列表里看不到任何基本面上下文。

## 铁律（本设计的硬约束）

**不产生"基本面 + 技术"的合成买入分。** 排名数字仍然只有现有的技术 `stock_score`；基本面 `Profile` 只作为旁证上下文展示。`screen.rs` 的"排除非选中 / 禁止 score 字段"原则原样保留。任何试图把 Profile 折算进排名分数的做法都违反本设计。

## 方案：screen 作为 recommend 的前置闸门（A1）

在技术排名**之前**加一道基本面闸门：先用 `screen::evaluate` 把不合格标的挡掉，再对幸存者跑现有技术排名，并把每只的 `Profile` 作为上下文附上。

### 数据流

```
STOCK_POOL 每只:
  ├─ A股  → 取 Listing(全集快照)+财报+估值 → screen::evaluate
  │           ├─ Err(Exclusion) ⇒ 记入 gate_excluded, 踢出池(不进技术排名)
  │           └─ Ok(Profile)    ⇒ 过闸, 带着 Profile 留下
  ├─ 港/美 → 不取基本面, 直接留下, 标 GateStatus::NotApplicable("非A股")
  └─ A股但基本面抓取失败 → 留下, 标 NotApplicable("数据获取失败, 未经闸门")
        ↓ 幸存者
   现有 evaluate_stock(技术) → rank_top   ← 这两步完全不改
        ↓
   报告 { top:[技术推荐 + Option<Profile> + GateStatus], gate_excluded:[(code,理由)], ... }
```

### 非 A 股处理（已定：方案 a）

基本面闸门数据只有 A 股齐全：

- **A 股**：全集快照（市值/PE/名称）+ 财报 + 估值历史全有 → 闸门完整生效。
- **港股**：有财报，但无估值历史、不在 A 股全集快照里 → 不过闸，标 NotApplicable。
- **美股**：财报/估值/快照都没有 → 不过闸，标 NotApplicable。

**决策**：非 A 股原样放行，但明确标注"基本面闸门不适用（非A股）"，风险由用户自负。不砍掉港美股选股能力。

### 失败放行（fail-open, 明确标注）

A 股若基本面/估值抓取失败（网络抖动等），**保留该股并标 `NotApplicable("数据获取失败, 未经闸门")`**，不因一次网络抖动误杀好票。代价：数据失败的股票会绕过闸门，但因有明确标注、且股票池是人工精选的预设池，可接受。

## 后端改动

### `src/stock/recommend.rs`

- 新增枚举 `GateStatus`：`Passed` / `NotApplicable { reason: String }`（`Serialize`）。
- 新增闭包返回类型 `GateOutcome`：`Passed(Profile)` / `Excluded(Exclusion)` / `NotApplicable(String)`。
- `StockRecommendation` 增字段：
  - `profile: Option<screen::Profile>` —— A 股过闸者 Some，其余 None。
  - `gate: GateStatus`。
- `StockRecommendReport` 增字段：
  - `gate_excluded: Vec<(String, String)>` —— (code, 排除理由)，被闸门在技术排名前踢掉的股票。
- `build_report` 增第二个注入闭包 `gate: FnMut(&str) -> GateOutcome`，保持纯逻辑可单测：
  - 遍历 pool：先调 gate。`Excluded` ⇒ 记 gate_excluded、跳过；`Passed`/`NotApplicable` ⇒ 该股进入现有 `load`→`evaluate_stock` 技术流程，并把 profile/gate 挂到产出的 `StockRecommendation` 上。
  - `rank_top` 只对幸存者排名，逻辑不变。
- 复用 `screen::evaluate`，**不重写**任何排除规则。
- `evaluate_stock` / `rank_top` / z-score / 技术评分逻辑**一行不动**。

### `src/web/stock.rs` `recommend_blocking`

- 按代码判市场：用 secid 解析，`secid.market` 0/1 = A、116 = 港、105/106/107 = 美。
- A 股：`universe::load_or_fetch`（全集快照，一次抓取全市场，共享）找 `Listing`；`fundamentals::load_or_fetch`、`valuation::load_or_fetch`（均带缓存，同 `screen_blocking`）；调 `screen::evaluate` 组装 `GateOutcome`。
- 港/美股：直接 `GateOutcome::NotApplicable("非A股")`。
- 组装 gate 闭包传入 `build_report`；bars 的 `load` 闭包保持现状。

## 前端改动

`src/web/page.rs`，"股选股" tab（渲染逻辑约 line 1159 起，调 `/api/stock/recommend`）：

- 推荐列表上方加一行闸门摘要：`基本面闸门排除 N 只：亏损 x·壳股 y·历史不足 z…`（读 `gate_excluded`，按理由聚合计数）。
- 每张推荐卡：
  - 若有 `profile`：加一条紧凑基本面上下文行 `ROE 连续 N 年≥15% · 净利5年CAGR x% · PE 自身历史分位 y%`（字段缺失的省略）。
  - 若 `gate` 为 `NotApplicable`：显示灰标 `基本面闸门不适用（非A股/数据缺失）`。
  - 保留现有技术 `stock_score` / `diagnosis` 渲染。
- 基本面行措辞沿用 screen 的"事实非预测"口径，不得暗示买入（不出现"推荐/买入/看好/目标价"等）。

## 测试

- **recommend.rs 单测**（沿用现有 IO 注入 fixture 风格）：
  - 闸门 `Excluded` 项进 `gate_excluded`，且不进技术排名（不出现在 `top`）。
  - 港/美股（NotApplicable）保留、带 `gate = NotApplicable`、`profile = None`。
  - A 股过闸带 `profile = Some(..)`、`gate = Passed`。
  - **不变量测试**：推荐 JSON 里排名数字仍只有技术 `stock_score`；Profile 是独立上下文，没有任何把它折算进排名的合成分字段。
  - 现有技术排名/切分/空池/序列化测试全部保持通过。
- screen.rs 现有测试不动（闸门复用其 `evaluate`，不改其逻辑）。

## 已知代价

选股首次运行会多抓 A 股的财报/估值（池约 8 只 A 股，均带缓存，二次秒级），与现有 `/api/stock/screen` 行为一致。

## 不做的事（YAGNI）

- 不做基本面+技术合成评分（违反铁律）。
- 不扩全市场离线批处理选股（另议）。
- 不改 `screen.rs` 的排除规则或其独立端点。
- 不动基金侧 `/api/recommend`。

## 涉及文件

- `src/stock/recommend.rs`（主要）
- `src/web/stock.rs`（`recommend_blocking` + 市场判定辅助）
- `src/web/page.rs`（"股选股" 渲染）
- 视需要 `src/stock/mod.rs` 的 re-export
