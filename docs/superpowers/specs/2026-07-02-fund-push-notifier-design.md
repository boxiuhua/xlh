# 定时推送模块 `push` 设计

日期：2026-07-02
状态：已批准，待实现

## 目标

新增一个可常驻的推送模块：按 `push.toml` 里的 **cron 表达式**定时触发，先**同步**配置好的基金最新净值，再生成**持仓建议 + 基金诊断说明**，通过**钉钉 / 飞书 / 企业微信 群机器人 或 Server酱（个人微信）**推送出去。

## 模块结构（`src/push/`，逻辑与 IO 分离）

- `mod.rs`：公开 `run_once(&PushConfig)` / `run_daemon(&PushConfig)` + 顶层类型。
- `config.rs`：`PushConfig` 及 `push.toml` 解析与校验。
- `message.rs`：把持仓报告 + 额外诊断 + 同步简报组装成 Markdown（纯函数，可测）。
- `channels.rs`：四渠道请求构造（URL/body/加签）+ 发送。构造与发送分离，便于测试。
- `schedule.rs`：cron 解析 + 守护循环（普通阻塞线程）。
- `job.rs`：一次任务编排：同步 → 建议/诊断 → 组装 → 发送。

## 依赖（新增，均轻量）

- `cron = "0.12"`：cron 表达式解析（配合已有 chrono 计算下次触发）。
- `hmac = "0.12"` + `sha2 = "0.10"` + `base64 = "0.22"`：钉钉/飞书加签（HMAC-SHA256）。

## 配置 `push.toml`

```toml
[schedule]
cron = "0 30 8 * * *"          # 6 段含秒：每天 08:30:00

[channel]
kind = "feishu"                # dingtalk | feishu | wework | serverchan
webhook = "https://open.feishu.cn/open-apis/bot/v2/hook/xxx"
secret = ""                    # 可选：钉钉/飞书加签密钥；serverchan 时 webhook 填 sendkey
cache_dir = ".cache"

[portfolio]                     # 均选填，回显
total_amount = 30000
total_profit = 1800
cumulative_profit = 4200

[[holdings]]                    # 复用 holdings::Holding
code = "161725"
amount = 12000
profit = 900

diagnose = ["110022"]           # 额外只诊断、不持有的基金（选填）
```

`PushConfig` 结构：

```
PushConfig {
    schedule: ScheduleCfg { cron: String },
    channel: ChannelCfg { kind: String, webhook: String, secret: String, cache_dir: PathBuf },
    portfolio: PortfolioCfg { total_amount/total_profit/cumulative_profit: Option<f64> },
    holdings: Vec<holdings::Holding>,   // 复用
    diagnose: Vec<String>,
}
```

校验：`kind` 必须是四者之一；`webhook` 非空；`cron` 能被 `cron::Schedule::from_str` 解析。校验失败在加载时即报错。

## 任务流程（`job::run`）

1. **同步**：holdings 的 code ∪ diagnose 去重，对每只 `data::sync::sync_fund(code, cache_dir)`，收集 `SyncOutcome` 作同步简报。
2. **建议**：由配置构造 `holdings::HoldingsInput`，8 年窗口 `cache::load_or_fetch` 注入，`holdings::build_report` 出逐只操作建议（含形态诊断）。
3. **额外诊断**：对 `diagnose` 每只加载净值 → `analyze::detect_regime_with_plan(points, &RegimeParams::default(), &PlanParams::default())`，得诊断说明。
4. **组装**：`message::compose(report, diags, sync_notes)` → Markdown：组合汇总 / 逐只（动作+建议金额+择时+最优策略）/ 额外诊断 / 同步简报 / 免责声明。
5. **发送**：`channels::send(&channel, title, &md)`。

错误处理：单只同步/加载/诊断失败 → 在消息内标注、不中断整体；发送失败 → `run_once` 返回 Err（守护循环里记日志后继续下次）。

## 渠道与加签（`channels.rs`）

统一 `build_request(cfg, title, md, ts_millis) -> HttpReq { method, url, body }`（纯函数，可测），`send` 用 reqwest blocking 执行。

- **钉钉**：`{"msgtype":"markdown","markdown":{"title","text"}}`；有 `secret` 则 `sign = base64(HMAC_SHA256(secret, "{ts}\n{secret}"))`，URL 追加 `&timestamp={ts}&sign={urlencode(sign)}`。
- **飞书**：`{"msg_type":"text","content":{"text": md}}`；有 `secret` 则 body 加 `timestamp`、`sign = base64(HMAC_SHA256("{ts}\n{secret}", ""))`。
- **企业微信**：`{"msgtype":"markdown","markdown":{"content": md}}`，无加签。
- **Server酱**：POST `https://sctapi.ftqq.com/{webhook}.send`，表单 `title`、`desp=md`。

发送后校验 HTTP 2xx 且（能解析时）响应体的 `errcode/code == 0`，否则返回 Err 带响应片段。

## 命令（`main.rs` 新增子命令）

```
xlh push --config push.toml         # 默认守护：解析 cron，循环 sleep 到点触发
xlh push --config push.toml --once  # 立即跑一次即退出（测试/交给系统调度）
```

`--config` 默认 `push.toml`。守护循环：`Schedule::from_str` → `loop { 取 upcoming 下次；sleep 到点；run_once；}`；`run_once` 出错仅打印日志不退出。守护为独立阻塞线程，不引入 tokio。

## 测试

- `config.rs`：解析完整 `push.toml`；非法 `kind`/空 `webhook`/坏 cron 报错。
- `message.rs`：给定持仓报告 + 诊断 + 同步简报，Markdown 含各区块关键字（汇总、动作、建议金额、免责声明）。
- `channels.rs`：四渠道 `build_request` 的 URL/body 正确；钉钉/飞书加签对固定 `secret`+`ts` 产出稳定 sign（HMAC 向量）。
- `schedule.rs`：`Schedule::from_str` 对给定表达式，从固定时刻算出的下次触发符合预期。
- 发送 IO 与守护 sleep：保持薄，不做单测。

## 范围外（YAGNI）

- 不做 Web UI 管理推送；不做多任务/多渠道并存（单配置单渠道）；不持久化发送历史（重启后按 cron 取下次即可，无重复风险）。
- 个人微信仅经 Server酱，不做公众号模板消息。
