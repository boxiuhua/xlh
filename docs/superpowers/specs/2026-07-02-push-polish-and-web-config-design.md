# 推送打磨 + Web 推送配置 Tab 设计

日期：2026-07-02
状态：已批准，待实现

在既有 `push` 模块（`feat(push)` 已提交）基础上做四件事：无新数据跳过、发送重试、股票持仓纳入推送、Web「推送」配置 Tab。

## A. 无新数据跳过（避免周末/节假日空推）

- `ScheduleCfg` 增 `only_on_new_data: bool`（`#[serde(default = default_true)]`，默认 true）。
- `job::run`：同步（基金+股票）后 `has_new = 任一 SyncOutcome.added > 0`。若 `only_on_new_data && !has_new` → 打印「无新数据，跳过推送」并 `return Ok(())`，不发送。
- 以数据新鲜度为准，天然规避非交易日，无需节假日日历。

## B. 发送失败重试

- `channels::send` 内对 POST 失败/HTTP 非 2xx 重试至多 3 次、每次间隔 5s，全败返回最后错误。构造函数 `build_request` 不变（仍纯函数可测）。

## C. 股票持仓纳入推送

- 配置：`push.toml` 增 `[[stocks]]`（复用 `holdings::Holding`：code/amount/profit）与根级 `diagnose_stocks: Vec<String>`。
- 新增 `src/push/stock_advice.rs`：
  - `StockAdvice { code, name, amount, profit, action, suggest_amount, signal, trend, z, price, adj_price, rsi, rationale }`。
  - `advise(h: &Holding, diag: &StockDiagnosis) -> StockAdvice`：动作规则同基金——`trend=="下跌"`→观望（`boll_z ≤ -1.5` 时深跌小额加仓，pct 减半）；`signal` 含「买入」→加仓；含「卖出」→ `profit>0` 止盈 / 否则 减仓；其余→持有。额 = `amount × clamp(0.10 + 0.10·min(|z|,2), 0.10, 0.30)`，`z = boll_z`；`amount<=0` 时 suggest=0。
- 复用共享启发式：把 `holdings::size_pct` 提为 `pub`，stock_advice 复用；动作/金额与基金一致。
- `job::run`：股票缓存目录 = `cache_dir.join("stock")`。对 stocks ∪ diagnose_stocks 去重 `stock::data::sync::sync_stock` 同步；`stock::data::cache::load_or_fetch` 取 bars → `stock::diagnose::diagnose(code, name, &bars, &DiagnoseParams::default())`；持仓项 `advise`，`diagnose_stocks` 仅出诊断。
- 消息：`message::compose` 增「股票持仓建议 / 股票诊断」区块；同步简报合并基金+股票。

### message 重构

`compose` 改签名，接收统一入参：
```
compose(
  fund: &HoldingsReport,
  fund_diags: &[(String, String, RegimeReport)],
  stock_adv: &[StockAdvice],
  stock_diags: &[StockDiagnosis],
  sync: &[SyncNote],            // 归一后的同步行，含 code/added/latest/error
) -> String
```
`SyncNote { code, added, latest: Option<String>, error: Option<String> }` 在 message.rs 定义；job 把基金 `data::sync::SyncOutcome` 与股票 `stock::data::sync::SyncOutcome` 各映射成 `SyncNote`。

## D. Web「推送」配置 Tab（基金组下新增）

### 后端（web/mod.rs）

- `PushConfig` 及子结构加 `#[derive(Serialize)]`；`holdings::Holding` 加 `Serialize`。写盘用 `toml::to_string`。
- 路由：
  - `GET /api/push/config`：读工作目录 `push.toml`，返回 JSON；文件不存在则返回一份默认空配置（表单可空开）。
  - `POST /api/push/config`：body 为 `PushConfig` JSON → `config::validate` → `toml::to_string` 写 `push.toml`；成功返回 `{ok:true}`，校验失败 400。
  - `POST /api/push/preview`：body 为 `PushConfig` JSON → `job::build_message`（同步+组装、**不发送**）→ 返回 `{markdown}`。
  - `POST /api/push/test`：body 为 `PushConfig` JSON → `push::run_once`（含发送）→ 返回 `{ok, error?}`。
- `job` 抽出 `build_message(cfg) -> Result<(String, bool)>`（返回 markdown 与 has_new）；`run` = `build_message` +（`only_on_new_data && !has_new` 跳过）+ `channels::send`。preview 用 `build_message`，test 用 `run_once`。

### 前端（page.rs，基金组 Tab「推送」，`data-tab="push"`）

- 表单：渠道 `kind`(下拉 dingtalk/feishu/wework/serverchan)/`webhook`/`secret`；`cron`；组合总览(total_amount/total_profit/cumulative_profit)；基金持仓动态行(code/amount/profit)；股票持仓动态行(code/amount/profit)；基金诊断代码、股票诊断代码(逗号分隔输入)。
- 按钮：读取当前配置 / 保存 / 预览消息 / 立即推送。
- 预览以 `<pre>` 展示 Markdown 文本；保存/推送结果以提示行显示。
- 说明文案：Web 改的是 `push.toml`；后台 `xlh push` 守护进程**重启后生效**（不热重载）。

## 测试

- `config`：解析含 `[[stocks]]`/`diagnose_stocks`/`only_on_new_data` 的样例；序列化往返（to_string→from_str 等价）。
- `stock_advice`：买入→加仓且额>0；卖出+盈利→止盈；卖出+亏损→减仓；下跌非深跌→观望。
- `message`：含股票区块关键字；SyncNote 失败标注。
- `job`：`only_on_new_data` 且无新数据 → `build_message` 返回 has_new=false（跳过发送在 run 层）。
- `channels`：重试逻辑（构造仍纯函数，重试测试可选，保持薄）。
- web：`/api/push/config` 读写往返；`/api/push/preview` 返回非空 markdown；坏配置 `POST /api/push/config` 400。

## 范围外（YAGNI）

- 守护进程热重载 `push.toml`（重启生效）；多任务/多渠道；Web 端并发保护（本地单用户）。
