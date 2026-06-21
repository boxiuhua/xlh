# 回测结果 HTML 分析页面 —— 设计文档

- 日期：2026-06-15
- 状态：已定稿（用户授权自主交付，免审批）
- 依附项目：xlh A股基金回测引擎（feat/backtest-engine 分支）

## 1. 目标

在现有 Rust 回测引擎之上，生成一个自包含的交互式 HTML 分析报告，供用户在浏览器中查看与分析单次回测结果。双击打开即可，无需启动服务器。

非目标（YAGNI）：多回测对比、实时刷新、参数在线调节、后端服务、登录。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 形态 | Rust 引擎生成单个 report.html，数据 JSON 内嵌 | 无需服务器/前端构建链，双击即开 |
| 图表库 | ECharts（CDN 引入） | 金融图表能力强；本环境联网可用，数据已内嵌故仅图表库依赖网络 |
| 文本/表格 | Rust 服务端直接渲染进 HTML | 即使 JS/网络失败，指标与流水仍可读 |
| 数据捕获 | 引擎新增逐日记录与成交流水，保持 run()->&Portfolio 不变 | 不破坏既有测试与调用方 |
| 自测 | Rust 单测验证生成 + Playwright 验证渲染 | 既测数据正确性也测页面真的能渲染 |

## 3. 数据捕获（engine 改动）

新增 src/result.rs：

```rust
#[derive(Debug, Clone, Serialize)]
pub struct DailyRecord {
    pub date: NaiveDate,
    pub nav: f64,          // 单位净值（展示）
    pub adj_nav: f64,      // 复权净值（计价）
    pub equity: f64,       // 当日权益 = cash + shares*adj_nav
    pub contribution: f64, // 当日新增外部投入
    pub shares: f64,       // 当日持有份额
    pub cash: f64,         // 当日现金
}

#[derive(Debug, Clone, Serialize)]
pub struct TradeRecord {
    pub date: NaiveDate,
    pub direction: Direction, // 复用 event::Direction（加 Serialize, lowercase）
    pub shares: f64,
    pub price: f64,           // 成交复权价
    pub fee: f64,
}
```

Engine 新增字段 daily: Vec<DailyRecord>、trades: Vec<TradeRecord>，在 run() 中：
- Fill 事件：成交份额 > 1e-9 时 push 一条 TradeRecord（忽略零份额空成交）。
- 每日队列排空、record_equity 之后：读取 portfolio.curve.last() 取 equity/contribution，连同 today.nav/adj_nav、broker.total_shares()、portfolio.cash 组成 DailyRecord push。

新增只读访问：pub fn daily(&self)->&[DailyRecord]、pub fn trades(&self)->&[TradeRecord]。run() 返回类型与签名不变（既有测试不受影响）。

event::Direction 增加 #[derive(Serialize)] #[serde(rename_all="lowercase")]（产出 "buy"/"sell"）。

## 4. 报告渲染（report/html.rs）

```rust
pub struct ReportMeta {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub strategy: String,        // 策略 kind
    pub strategy_desc: String,   // 参数摘要
    pub initial_cash: f64,
}

/// 渲染自包含 HTML 报告到 out_dir/report.html，返回写入路径。
pub fn render_report(
    meta: &ReportMeta,
    pf: &Portfolio,
    daily: &[DailyRecord],
    trades: &[TradeRecord],
    out_dir: &Path,
) -> Result<PathBuf>;
```

流程：
1. 计算指标（复用 metrics::*）：累计投入、期末市值、总收益、年化(XIRR)、最大回撤、夏普、交易次数。
2. 组装可序列化 Payload { meta, metrics, daily, trades }，serde_json::to_string 生成数据块。
3. 注入 HTML 模板（服务端渲染指标卡片与成交表行 + 内嵌 <script>const DATA=...</script> + ECharts 初始化脚本），写文件。

页面结构：
- 顶部：标题（基金代码）、回测区间、策略与参数摘要。
- 指标卡片网格：总收益、年化、最大回撤、夏普、累计投入、期末市值、交易次数（颜色按正负）。
- 图表区（ECharts，从内嵌 JSON 构建）：
  - 权益曲线 + 累计投入曲线（差值即盈亏），带 dataZoom。
  - 回撤水下图（JS 由 equity 跑动峰值算出 area）。
  - 净值图：单位净值 + 复权净值双线，买点(▲红)/卖点(▼绿) 以 markPoint 标在对应日期的 adj_nav。
- 成交流水表：日期/方向/份额/价格/费用/金额（服务端渲染）。
- 健壮性：若 typeof echarts === 'undefined'（CDN 失败），图表区显示提示文字，文本与表格不受影响。

样式：浅色主题、系统字体、卡片式布局、响应式网格、克制配色（盈利红、亏损绿，遵循 A股习惯）。内联 <style>，无外部 CSS 依赖。

## 5. 配置与 CLI

- ReportCfg 增加 #[serde(default)] pub html: bool（默认 false）。
- main.rs：engine.run() 后，若 cfg.report.html，由 cfg 组装 ReportMeta，调用 render_report，打印输出路径。
- 示例 config.toml 增加 html = true。

## 6. 错误处理

render_report 全程 anyhow::Result；out_dir 创建失败、路径非 UTF-8、写文件失败均返回带上下文错误，不 panic。

## 7. 测试

- Rust 单测（report/html.rs）：构造小规模 pf+daily+trades，渲染到临时目录，断言：文件存在；含 const DATA；含基金代码；含各指标中文标签；JSON 中 daily 长度正确；含至少一条成交表行；含 ECharts 脚本引用。
- Playwright 渲染自测（webapp-testing）：用真实回测生成 report.html，file:// 打开，断言无 console error、ECharts 实例已初始化、截图留档。
- 既有 42 个测试必须全绿。

## 8. 依赖

无新增 Rust crate（serde/serde_json 已在）。前端仅 ECharts CDN。自测用 Playwright（webapp-testing 工具链）。
