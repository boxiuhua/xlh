# 本地回测 Web 界面（单次回测）—— 设计文档

- 日期：2026-06-19
- 状态：已定稿（用户逐步确认通过）
- 依附：xlh 回测引擎 + HTML 报告（已合入 master）

## 1. 目标

提供一个本地 Web 界面 `xlh serve`：在浏览器表单里选基金/区间/策略/参数，点"运行"由后端实时跑单次回测，并把现有的自包含 HTML 报告（收益曲线/回撤/指标卡片）刷新到页面，无需敲命令行、无整页刷新。

复用面最大：后端 = "接表单 → 调已有引擎 → 吐已有报告"；前端 = 表单 + iframe。引擎、config 构建、ECharts 报告全部不重写。

非目标（YAGNI）：对比/寻优模式入界面、登录鉴权、历史持久化、并发多用户、自定义卖出阶梯、生产部署/HTTPS/远程访问。首版 = 本地单机、单次回测、一个表单一张报告。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 交互形态 | 表单调参 → 后端实时重算 → 刷新报告 | 用户选定 |
| 范围 | 首版只做单次回测 | 用户选定；架构跑通后再加对比/寻优 |
| 后端 | axum + tokio，新增子命令 `xlh serve` | 轻量、单二进制；子命令保持 `--config` 路径向后兼容 |
| 渲染复用 | 抽 `render_report_html() -> String`，server 与文件输出共用 | DRY；不重写图表 |
| 前端取结果 | `iframe.srcdoc = 返回的报告 HTML` | 复用整份自包含报告，零图表重写，表单状态保留 |
| 绑定 | `127.0.0.1:8080`（端口可 `--port` 改） | 本地单机，不对外暴露 |
| 卖出费率 | 首版固定标准阶梯，不进表单 | YAGNI；表单只暴露 buy_rate |
| 阻塞调用 | `spawn_blocking` 包住 load/run | 同步 IO 不卡 async runtime |

## 3. CLI（main.rs 重构）

把 `Cli` 改为带可选子命令，保持无子命令时的既有行为：

```rust
#[derive(Parser)]
#[command(name = "xlh", about = "A股基金定投/择时回测")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// 配置文件路径（无子命令时使用）
    #[arg(short, long, default_value = "config.toml", global = true)]
    config: PathBuf,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动本地 Web 界面
    Serve {
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
}
```
- `command == None` → 现有 `--config` 回测流程（含 optimize/compare/单次分支），逻辑整体抽到 `fn run_cli(cfg)` 不变。
- `command == Some(Serve{port})` → 在该分支内**显式构建** tokio runtime 跑 `xlh::web::serve(port)`，使无子命令的同步 CLI 路径保持非 async（`main` 仍是同步 `fn main() -> Result<()>`）：
  ```rust
  Some(Commands::Serve { port }) => {
      let rt = tokio::runtime::Runtime::new()?;
      rt.block_on(xlh::web::serve(port))?;
      Ok(())
  }
  ```

## 4. 模块结构

- 重构 `src/report/html.rs`：
  ```rust
  /// 渲染单次回测报告为完整 HTML 字符串（自包含 ECharts）。
  pub fn render_report_html(
      meta: &ReportMeta,
      pf: &crate::portfolio::Portfolio,
      daily: &[crate::result::DailyRecord],
      trades: &[crate::result::TradeRecord],
  ) -> String;
  ```
  现有 `render_report(...)`（写文件）改为：`let html = render_report_html(...); 写盘`。`build_html` 仍私有，由 `render_report_html` 调用。页面文案与既有测试断言不变。
- 新增 `src/web/mod.rs`：`pub async fn serve(port: u16) -> anyhow::Result<()>`；axum `Router`；handlers；`AppError`；`RunQuery`；纯函数 `build_run_from_query`。
- 新增 `src/web/page.rs`：`pub const INDEX_HTML: &str`（表单页，HTML+CSS+JS 内联）。
- `src/lib.rs`：加 `pub mod web;`。
- 依赖（Cargo.toml）：`axum = "0.7"`、`tokio = { version = "1", features = ["rt-multi-thread", "macros"] }`、`tower = "0.5"`（测试用 oneshot；如 axum 自带可省）。

## 5. 路由与契约

- `GET /` → `Html(INDEX_HTML)`（200, text/html）。
- `GET /api/run` → query 反序列化为 `RunQuery` → `build_run_from_query` → `spawn_blocking`(load + run) → `render_report_html` → `Html(report)`（200）。出错 → `AppError` → 400 + 中文错误 HTML（iframe 内可读）。

`RunQuery`（所有策略字段并集，按 strategy 取用）：
```rust
#[derive(Debug, Deserialize)]
pub struct RunQuery {
    pub fund_code: String,
    pub start: chrono::NaiveDate,
    pub end: chrono::NaiveDate,
    pub strategy: String,                 // dca | smart_dca | trend
    #[serde(default)] pub buy_rate: f64,
    #[serde(default)] pub initial_cash: f64,
    // dca / smart_dca
    #[serde(default)] pub period: Option<String>,
    #[serde(default)] pub day: Option<u32>,
    #[serde(default)] pub base_amount: Option<f64>,
    // smart_dca
    #[serde(default)] pub ma_window: Option<usize>,
    #[serde(default)] pub k: Option<f64>,
    // trend
    #[serde(default)] pub short_window: Option<usize>,
    #[serde(default)] pub long_window: Option<usize>,
    #[serde(default)] pub amount: Option<f64>,
}
```

## 6. 纯函数 build_run_from_query

```rust
pub struct RunSpec {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub strategy: Box<dyn Strategy>,
    pub fee: FeeModel,
    pub initial_cash: f64,
}

/// 校验 query 并组装回测所需的一切；不做任何 IO。
pub fn build_run_from_query(q: &RunQuery) -> anyhow::Result<RunSpec>;
```
逻辑：
1. `start < end` 否则报错。
2. 按 `q.strategy` 从相关字段拼一个 `toml::Table`（缺必填字段报错，含字段名），转 `toml::Value`，调 `config::build_strategy_from(&q.strategy, &Some(params), &[])`。
3. `fee = FeeModel { buy_rate: q.buy_rate, sell_tiers: 标准阶梯 }`，标准阶梯 = `[{7,0.015},{365,0.005},{0,0.0}]`（与示例配置一致）。`buy_rate` 须在 `[0,1)`，否则报错。
4. 返回 `RunSpec`。

注：`build_strategy_from` 已对 smart_dca.ma_window<1、trend.short<1 与 short>=long 等做校验，错误会带中文说明透传。

## 7. 数据流（/api/run）

1. 浏览器表单提交（JS）→ `GET /api/run?fund_code=…&start=…&strategy=…&…`。
2. handler 反序列化 `RunQuery`（失败 → 400「参数解析失败」）。
3. `spec = build_run_from_query(&q)?`（失败 → 400 带原因）。
4. `spawn_blocking`：`points = cache::load_or_fetch(&spec.fund_code, Path::new(".cache"), spec.start, spec.end)?`（缺失自动联网；失败 → 400「加载净值失败: …」）。
5. 同一 blocking 闭包内 `outcome = runner::run_one(name, fund, points, strategy, fee, initial_cash)`。注意 `run_one` 当前返回 `RunOutcome`（含 summary+daily），但报告渲染需要 `Portfolio`/`trades`。见 §8。
6. `render_report_html(&meta, pf, &daily, &trades)` → String。
7. `Ok(Html(html))`。

## 8. 渲染所需数据（关键实现点）

`report::html::render_report_html` 需要 `&Portfolio`、`&[DailyRecord]`、`&[TradeRecord]`，而 `runner::run_one` 只回 `RunOutcome{summary, daily}`，不暴露 portfolio/trades。为不破坏 `run_one` 且拿到全部数据，web handler **直接装配引擎**（与 `run_one` 同样的安全借用顺序），不经 `run_one`：
```rust
let data = InMemoryData::new(points);
let broker = Broker::new(spec.fee);
let portfolio = Portfolio::new(spec.initial_cash);
let mut engine = Engine::new(data, spec.strategy, broker, portfolio);
engine.run();
let html = render_report_html(&meta, engine.portfolio(), engine.daily(), engine.trades());
```
即 web 复用 `Engine` 而非 `run_one`（`run_one` 是为对比/寻优批量场景做的摘要版）。这是有意决策：单次报告需要完整 portfolio/trades，引擎访问器 `portfolio()/daily()/trades()` 已是 `&self` 公共方法。

## 9. 错误处理

```rust
struct AppError(anyhow::Error);
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = format!("<!doctype html><meta charset=utf-8><body style=\"font-family:sans-serif;padding:24px;color:#c0392b\"><h3>回测失败</h3><pre>{}</pre>",
            crate::report::html_escape(&self.0.to_string()));
        (StatusCode::BAD_REQUEST, Html(body)).into_response()
    }
}
impl<E: Into<anyhow::Error>> From<E> for AppError { fn from(e: E) -> Self { Self(e.into()) } }
```
错误信息经 `html_escape`，iframe 内直接显示原因。

## 10. 前端（page.rs，INDEX_HTML）

单页：左/上为表单卡片，下为 `<iframe id="result">`。
- 字段：基金代码(text)、起始日/结束日(date)、策略(select: 普通定投/智能定投/均线择时)、随策略显隐的参数组、买入费率、初始现金。
- 策略 `<select>` 的 `change` 事件切换显示对应参数组（三个 `<div class="params" data-for="dca|smart_dca|trend">`）。
- "运行"按钮：JS 收集表单 → `URLSearchParams` → `fetch('/api/run?'+qs)` → `text()` → `iframe.srcdoc = text`；运行中显示 loading、禁用按钮。
- 合理默认值：fund_code=161725，区间 2020-01-01~2024-12-31，strategy=smart_dca，period=monthly、day=1、base_amount=1000、ma_window=250、k=1.0，buy_rate=0.0015，initial_cash=0。
- 样式沿用报告设计语言（卡片/浅色/红涨绿跌）。纯静态，无外部依赖（图表在 iframe 内的报告里）。

## 11. 测试

- **单元（web）**：`build_run_from_query`
  - dca/smart_dca/trend 各给全字段 → `is_ok()`；
  - start>=end → err 含 "start" 或日期提示；
  - 未知 strategy → err；
  - buy_rate>=1.0 → err 含 "buy_rate"；
  - smart_dca 缺 ma_window → err（透传 build_strategy_from 校验）。
- **路由**：`GET /` 经 axum `oneshot` → 200，body 含表单关键标识（如 `id="result"`、`name="fund_code"`、`运行`）。不需引擎/网络。
- **Playwright e2e**（scripts/verify_web.py）：后台启动 `xlh serve --port <p>`，等待端口就绪，访问 `http://127.0.0.1:<p>/`，填默认表单点"运行"，等 iframe 内出现 `canvas` 且含文本"总收益"，无 console error，截图 `output/web_screenshot.png`，最后杀进程。（基金 161725 已缓存，离线可跑。）
- 既有 64 测试保持绿、clippy 干净。

## 12. 单元边界小结

- `build_run_from_query`：纯校验+组装，无 IO，独立可测。
- `web::serve`/handlers：编排（解析 → 纯函数 → spawn_blocking 跑引擎 → 渲染），薄。
- `report::html::render_report_html`：纯渲染。
- `web::page::INDEX_HTML`：纯静态前端。
- `main.rs`：仅分流（子命令 vs 配置文件）。
各单元经 `RunQuery`/`RunSpec`/字符串 HTML 通信，可分别理解与测试。
