# xlh

基金投资理财研判系统 —— A股基金定投/择时回测、参数寻优与市场状态诊断；并扩展支持**股票（A股/港股/美股）**的行情抓取、单股回测、技术诊断与跨股选股。

## 运行与部署

本项目是 **Rust 单体程序**（一个可执行文件 `xlh` + 库）：Web 界面为**内嵌的 axum 服务**，前端 HTML 直接编译进二进制（`web/page.rs` 的 `INDEX_HTML`），无独立前端构建、无外部静态资源。本质是本地桌面工具，也可容器化部署到服务器（见下文「Docker 部署」）。

### 运行

```bash
cargo run --release -- serve          # 启动本地 Web 界面（默认 http://127.0.0.1:8080）
cargo run --release -- serve --port 9000
cargo run --release -- -c config.toml # 无子命令时按配置文件跑 CLI 回测/对比/寻优
cargo run --release -- push           # 按 push.toml 的 cron 定时推送持仓建议+诊断
cargo run --release -- push --once    # 立即推送一次（测试用）
```

`serve` 启动后浏览器打开 `http://127.0.0.1:8080`，即「基金 / 股票」两级 Tab 界面。

### 定时推送（钉钉 / 飞书 / 企业微信 / Server酱）

`xlh push` 按 `push.toml` 的 **cron 表达式**常驻：到点先**同步**配置的基金/股票最新数据，再生成**持仓建议（基金+股票）+ 诊断**，推送到群机器人或个人微信。复制 `push.toml.example` 为 `push.toml` 填好即可，**或直接在 Web 页面「基金 → 推送」Tab 图形化配置**（`push.toml` 已在 `.gitignore`，避免密钥入库）。

- **渠道**：`kind = dingtalk | feishu | wework | serverchan`。前三者为群机器人 webhook（POST JSON，免费无审核）；`serverchan` 走 Server酱推个人微信（`webhook` 填 sendkey）。钉钉/飞书填 `secret` 即启用 HMAC-SHA256 加签，留空则用「关键词/IP 白名单」安全模式。发送失败自动重试 3 次。
- **内容**：基金持仓 `[[holdings]]` + 股票持仓 `[[stocks]]`（含加仓/持有/减仓/止盈/观望及建议金额）+ 额外诊断 `diagnose` / `diagnose_stocks`。
- **cron**：6 段含秒（秒 分 时 日 月 周），如 `0 30 8 * * *` = 每天 08:30:00。`only_on_new_data`（默认 true）在无新数据时跳过，天然规避周末/节假日空推。
- **Web 配置 Tab**：「基金 → 推送」可编辑渠道/持仓、**预览消息**、**立即推送**；保存写入 `push.toml`，后台 `xlh push` 守护进程**重启后生效**（不热重载）。
- **调度进程**：`push` 为独立阻塞守护（非 `serve` 内）；`--once` 跑一次即退出，便于测试或交给系统计划任务。
- **TOML 提醒**：根级键 `diagnose` / `diagnose_stocks` 须写在 `[[holdings]]` / `[[stocks]]` 之前，否则会被并入数组表（用 Web Tab 配置则无此坑）。

### 部署（脱离 cargo）

编译为自包含单文件二进制，拷贝即可运行：

```bash
cargo build --release
./target/release/xlh serve --port 8080   # Windows 为 target/release/xlh.exe
```

- **无运行时依赖**：TLS 用 `rustls`（纯 Rust，不依赖系统 OpenSSL）；前端内嵌，无需 Node/静态目录。
- **需外网访问**：联网抓取腾讯 `web.ifzq.gtimg.cn`、东财 `fund.eastmoney.com` 等；缓存写入**工作目录下的 `./.cache/`**（详见「数据存储」），请在期望存放缓存的目录里启动。

### 注意

- **默认仅监听 `127.0.0.1`**（`web/mod.rs` 中 `serve`），只能本机访问。若要对局域网/服务器提供，设环境变量 **`XLH_BIND=0.0.0.0`**（Docker 镜像内已默认置为 `0.0.0.0`）。
- **无鉴权**：接口裸暴露，勿直接挂公网；确需对外请在前面套反向代理（nginx/Caddy）加认证。

### Docker 部署

仓库已含 `Dockerfile`（多阶段构建，运行镜像仅约 150MB）、`docker-compose.yml`（本地开发）、`docker-compose.prod.yml`（线上，仅运行不编译）、`scripts/deploy.sh`（一键发到服务器）。

> 说明：镜像里**不含** `config.toml` / `push.toml`（含密钥），一律运行时挂载；`.cache/`、`output/` 也挂卷持久化。plotters 画 PNG 需要的 `freetype/fontconfig`+字体已装进运行镜像；TLS 用 rustls，无需 OpenSSL。

**本地构建与运行**

```bash
docker build -t xlh:latest .
docker compose up -d xlh-web                    # Web 界面 → http://localhost:8080
docker compose --profile push up -d xlh-push    # 定时推送守护（按需）
docker compose logs -f xlh-web
```

**发到线上服务器（免镜像仓库，save + scp + load）**

Git Bash 里一键：

```bash
scripts/deploy.sh user@服务器IP                 # 加 WITH_PUSH=1 同时起推送守护
```

或手动（本机 **PowerShell** 用 `docker save -o`，别用 `| gzip >`，PowerShell 无 gzip 且 `>` 会损坏二进制）：

```powershell
docker save xlh:latest -o xlh-latest.tar
scp xlh-latest.tar docker-compose.prod.yml config.toml push.toml user@服务器IP:/opt/xlh/
```

```bash
# 服务器（Linux）
cd /opt/xlh && mkdir -p .cache output
docker load -i xlh-latest.tar
docker compose -f docker-compose.prod.yml up -d
```

**对外暴露**：`docker-compose.prod.yml` 默认把端口绑在 `127.0.0.1:8080`（防裸奔）。要开外网访问，改成 `- "8080:8080"` 后 `up -d`，**并在云安全组/防火墙放行入方向 TCP 8080**（来源建议限成你自己的 IP）。因界面无鉴权且 `push.toml` 含密钥，长期对外强烈建议前置 Nginx/Caddy + Basic Auth + HTTPS，只对外开 443。

**常见坑**

- **`写入 push.toml 失败: Is a directory`**：启动容器时宿主 `push.toml` **文件不存在**，Docker 会把挂载源自动建成**目录**。修复：`docker compose ... down` → `rm -rf push.toml` → 重新 `scp` 真正的文件 → 确认 `ls -l push.toml` 是文件 → `up -d`。**务必先放好文件再 `up`**（`config.toml` 同理）。
- **外网打不开、`docker ps` 显示 `127.0.0.1:8080->8080`**：端口只绑了本机，按上面「对外暴露」改绑定并放行安全组。
- **Windows Docker Desktop 本机 `localhost:8080` 返回 502**：是 Docker Desktop 的 WSL2 端口转发问题（对所有容器都复现，与本项目无关），Linux 服务器上不存在。

### 授权与收费

本项目内置 **SaaS 授权体系**，支持在线激活、到期宽限与功能锁定。

**首次部署：创建管理员账户**

```bash
XLH_ADMIN_PASSWORD=YourSecurePassword123 xlh admin create --username admin

docker exec -e XLH_ADMIN_PASSWORD='123qwe!@#QWE' xlh-web xlh admin create --username admin
```

启动 Web 后访问 `/admin`（左上管理后台入口），用上述凭证登录。

**发码与激活**

1. **命令行发码**（推荐自动化集成）：
   ```bash
   xlh license issue --days 365 --count 10
   ```
   生成 10 张 1 年有效期授权码，输出到 stdout（可重定向或复制分发）。

2. **网页后台发码**（交互式）：
   登入 `/admin` 后台 → 「授权码管理」tab → 输入有效期/数量 → 生成后在页面 `<pre>` 区手动复制授权码，线下发给客户（无自动邮件发送/下载功能）。

**客户激活流程**

1. **注册**：Web 首页「注册」tab（若服务端启用开放注册 `open_registration = true`）；若关闭开放注册，则由管理员通过 CLI/后台手动建号。
2. **激活**：登录后顶栏出现「未激活」提示，输入授权码 → 立即激活。
3. **到期前提醒**：顶栏倒数提示「还剩 X 天」（可配 `warn_days`，默认 7 天）。
4. **到期后宽限**：进入 `grace_days` 宽限期（默认 3 天）期间正常使用，顶栏警告「宽限中」。
5. **宽限结束锁定**：超期后无法访问核心功能（回测、推送、股诊等），仍可登录、查看授权状态、输入新授权码激活/续期、退出登录。

**持久化配置**

授权库与会话数据存储在**运行目录下的 `data/xlh.db` 文件**（SQLite）；Docker 部署时务必挂卷持久化：

```yaml
# docker-compose.yml / docker-compose.prod.yml
volumes:
  - ./data:/app/data    # 授权库、会话数据必须持久化
```

如容器重启或迁移，勿丢失此目录，否则所有授权码与用户登录状态丧失。

**配置选项**（`config.toml` 中可选，缺省用默认值）

```toml
[auth]
db_path = "data/xlh.db"           # SQLite 库路径（默认）
open_registration = true           # 允许网页注册（false 则关闭自助注册，需管理员手动建号）
warn_days = 7                       # 到期前多少天开始顶栏倒数
grace_days = 3                      # 超期后宽限期天数
session_ttl_days = 30              # 会话 Cookie 有效期（天）
```

---

## 数据存储

抓取的行情/净值以 **CSV 缓存在本地 `.cache/` 目录**（相对运行 `xlh` 的工作目录，如项目根 `./.cache/`）。基金与股票**分目录**存放；命中缓存且覆盖所请求日期区间即离线读取，否则重新抓取并覆盖写回，故第二次查同一标的为秒级。

| 类型 | 目录 | 文件名 | CSV 表头 |
|------|------|--------|----------|
| 基金净值 | `.cache/` | `{基金代码}.csv`（如 `161725.csv`） | `date,nav,acc_nav`（单位净值, 累计净值） |
| 基金清单 | `.cache/` | `fundlist.json` | 代码↔中文名映射（前端自动补全用） |
| 股票行情 | `.cache/stock/` | `{市场号}_{代码}.csv`（如 `1_600519.csv`） | `date,open,high,low,close,volume,adj_close`（`close` 不复权价, `adj_close` 后复权价） |

- **市场号**：`1`=沪、`0`=深、`116`=港股、`105/106/107`=美股（纳斯达克/纽交所/美交所）。股票用「市场号_代码」命名，避免三市场数字重码，也让基金「同步全部」不会误扫股票文件。
- **增量同步**：`/api/sync`（基金）、`/api/stock/sync`（股票）只把「晚于缓存最后一天」的新数据追加进已有 CSV。
- 数据源（免费无 key）：基金净值走**东方财富**（`fund.eastmoney.com`）；股票 K 线以**腾讯**（`web.ifzq.gtimg.cn`）为主、东财 `push2his` 为兜底。缓存目录当前固定为 `.cache`（如需迁移到数据盘或改用数据库，可后续做成可配置）。

> 注：东财 `push2his.eastmoney.com` 在个别网络 TLS 握手后即被服务端断开（IPv4/IPv6 皆然），故股票 K 线改以腾讯为主源、东财兜底。腾讯的**后复权**（`hfqday`）仅覆盖约近 2.5 年且仅 A 股有；港股/美股无复权数据，`adj_close` 回退为不复权 `close`；A 股为避免复权尺度断层，仅保留后复权覆盖区间。

---

## 技术架构

### 总览

xlh 是一个用 Rust 编写的**事件驱动回测引擎**，外层提供 CLI 与本地 Web（Axum）两套入口，共享同一套回测内核。整体分为「数据 → 策略 → 引擎撮合 → 组合记账 → 指标 → 报告」的单向数据流，辅以参数寻优与市场状态诊断两条旁路。

```
                 ┌────────── 入口层 ──────────┐
   CLI (main.rs) │                            │ Web (web/, Axum + Tokio)
                 └─────────────┬──────────────┘
                               ▼
        ┌──────────────── 回测内核 ────────────────┐
        │  data → strategy → engine → broker        │
        │                        → portfolio        │
        │                        → metrics          │
        └───────────────────────────────────────────┘
                               ▼
         report (html / chart / compare / optimize)
```

### 事件驱动管线

回测核心是一条四级事件流水线（`event.rs` / `engine.rs`）：

```
MarketEvent ──strategy──▶ SignalEvent ──portfolio──▶ OrderEvent ──broker──▶ FillEvent
   行情到达               策略产生买卖信号           风控转为确定订单         撮合扣费成交
```

`Engine`（`engine.rs`）每个交易日取一根 bar，压入事件队列，循环消费直至队列清空，再记录当日权益。各级职责清晰、互不耦合：

- **Market → Signal**：策略 `on_market` 基于「截至当日」的历史窗口生成信号；
- **Signal → Order**：`Portfolio::on_signal` 做基本风控（空仓不可卖、比例/现金额转份额），生成确定订单；
- **Order → Fill**：`Broker::execute` 按当日复权价撮合并扣费；
- **Fill**：`Portfolio::apply_fill` 记账，`Engine` 收集成交明细。

> **防偷看未来（look-ahead bias）**：`DataHandler::history` 只返回已发出的当日及之前的 bar，绝不暴露未来数据（见 `data/mod.rs` 的 `history_never_returns_future` 测试）。

### 模块分层

| 模块 | 职责 |
|------|------|
| `event.rs` | 四类事件（Market/Signal/Order/Fill）及 Direction、SignalAmount、OrderQty 等值类型 |
| `data/` | 数据层：`eastmoney` 抓取净值、`cache` CSV 本地缓存、`sync` 增量同步、`fundlist` 基金清单、`InMemoryData` 回放；并由单位净值+累计净值推导**复权净值**（隐含红利再投） |
| `strategy/` | 策略层：`Strategy` trait + 五种策略（`dca` 普通定投、`smart_dca` 智能定投、`trend` 均线择时、`rsi` 超买超卖、`adaptive` 自适应）；`RuleLayer` 以装饰器叠加止盈/止损 |
| `broker.rs` | 撮合与费用：FIFO 份额批次（lots）、买入费率、按持有天数分档的卖出阶梯费率 |
| `portfolio.rs` | 组合记账：现金、累计投入、权益曲线、XIRR 现金流（投入为负、期末市值为正） |
| `engine.rs` | 事件循环引擎，泛型于 `DataHandler` 与 `Strategy` |
| `metrics.rs` | 指标：总收益、最大回撤、夏普、XIRR（二分法求根的货币加权年化） |
| `analyze.rs` | 市场状态诊断（上升/下降/震荡）+ 均线±kσ 波动带的「高抛低吸」分档行动计划 |
| `optimize.rs` | 参数寻优：网格笛卡尔积展开 → 批量回测 → 按指标排序取 Top-N |
| `runner.rs` | 单次命名回测装配（data→engine→run→汇总） |
| `report/` | 报告：`html` 单次报告、`chart` 权益曲线图（plotters）、`compare` 多策略对比、`optimize` 寻优结果 |
| `config.rs` | TOML 配置解析与策略构建（`build_strategy_from`） |
| `stock/` | 股票体系（与基金业务代码互不 `use`，仅共用通用引擎）：`data/`（腾讯为主·东财兜底 K 线抓取/secid 三市场映射/后复权/CSV 缓存/搜索/同步 + `StockData` 引擎适配器）、`fee`（佣金+印花税+过户费）、`backtest`（单股回测）、`indicators`+`diagnose`（MA/MACD/布林/RSI 技术诊断）、`recommend`（跨股选股排名） |
| `web/` | Axum HTTP 服务 + 内嵌单页 HTML（`page.rs`）；组合根，同时接入基金与股票 |

### 关键设计

- **trait 抽象 + 泛型引擎**：`Engine<D: DataHandler, S: Strategy>` 对数据源与策略零成本泛型；`Box<dyn Strategy>` 也实现 `Strategy`，便于 `RuleLayer` 包裹与运行期组合。
- **策略装饰器**：`RuleLayer` 包裹任意内层策略，在其信号之上追加止盈（`TakeProfit`）/止损（`StopLoss`）清仓信号，正交于具体策略。
- **纯函数 + IO 分离**：Web 层把「校验+组装」（`build_run_from_query` 等纯函数，无 IO）与「加载数据+跑回测」分离；非 `Send` 的 `Box<dyn Strategy>` 在 `spawn_blocking` 线程内创建并消费，不跨 `await`。
- **缓存优先的数据获取**：`cache::load_or_fetch` 命中本地 CSV 则直接读，否则向天天基金抓取；`sync` 支持只追加「晚于缓存最后一天」的增量点。
- **安全**：基金代码白名单校验（拒绝路径穿越），HTML 输出统一转义。

### Web 接口

`web/mod.rs` 的 `router()` 暴露：

| 路由 | 方法 | 用途 |
|------|------|------|
| `/` | GET | 单页界面（基金：单次/对比/寻优/诊断/推荐；股票：股诊断/股回测/股选股） |
| `/api/run` | GET | 单次回测，返回 HTML 报告 |
| `/api/compare` | POST | 多策略对比 |
| `/api/optimize` | POST | 参数网格寻优 |
| `/api/regime` | GET | 市场状态诊断 + 高抛低吸行动计划（JSON） |
| `/api/funds` | GET | 基金清单（前端联想搜索） |
| `/api/sync` | POST | 净值数据增量同步 |
| `/api/stock/search` | GET | 股票代码/名称搜索（前端联想，跨三市场） |
| `/api/stock/diagnose` | GET | 单股技术诊断（趋势 + MA/MACD/布林/RSI 综合信号，JSON） |
| `/api/stock/run` | GET | 单股回测（费率按市场自动选，JSON 绩效+交易统计） |
| `/api/stock/recommend` | GET | 跨股选股：多策略样本外评分 + z-score 排名 Top-N |
| `/api/stock/sync` | POST | 股票行情增量同步 |

### 技术栈

Rust 2021 · axum 0.7 · tokio · reqwest（rustls）· plotters · clap · serde/serde_json · toml · chrono · anyhow/thiserror。
