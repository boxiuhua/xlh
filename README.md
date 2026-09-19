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

### 美股分析

预测结果现支持[实际预测记录与到期验证](docs/forecast-tracking.md)：首次概率与模型输入永久保存，到期自动核验，并显示命中率、同样本基准和概率校准检查。历史回放与真实登记记录分别展示；尚无合格到期样本时显示未知。

「股票 → 诊断」和「股票 → 回测」均可选择沪深、美股、港股，默认沪深。搜索结果按所选市场过滤，切换时清空旧代码和结果，服务端拒绝代码与市场不一致的请求。诊断结果明确标注市场（沪深进一步区分沪市、深市）与计价币种；回测初始资金也按市场标注币种。

诊断结果包含历史趋势图：最近 60 / 120 / 250 个交易日及全部已加载历史，展示价格、MA20、MA60；支持鼠标查看和左右方向键逐日查看。可切换原始收盘价与指标用价，均线按相同口径计算。“全部”指当前诊断加载的历史窗口，不代表上市以来全部行情。

选择「美股」后可点击快捷入口（AAPL、MSFT、NVDA、TSLA、AMZN、GOOGL），或输入 ticker / `us.TSLA`。结果提供美元价格、行情日期、MA/MACD/布林/RSI、方向模型和历史信号检验。数据源搜索必须精确匹配代码，找不到时返回错误，不替换成其他股票。

这是日线分析，行情日期以数据源交易日为准，不含实时、盘前盘后行情。未确认复权时会明确标注，拆股、分红可能影响指标与回测；当前美股尚无市场基准过滤、基本面筛选和盘中异动监控。方向模型的概率是启发式结果，需结合样本外证据查看。

可运行 `cargo test --test us_stock_analysis -- --include-ignored --nocapture` 检查真实行情与诊断链路。该检查将 AAPL、NVDA、TSLA 的日线和诊断 JSON 保存到 `data/us_stock_analysis/`，不会执行交易或推送。

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

### 更新命令
 cd /opt/xlh
  docker load -i 新镜像包.tar.gz
  docker compose -f docker-compose.prod.yml --profile push up -d --force-recreate

**发到线上服务器（免镜像仓库，save + scp + load）**

Git Bash 里一键（脚本自带：部署前备份、旧布局迁移、崩溃循环检测、部署前后用户数比对）：

```bash
WITH_PUSH=1 scripts/deploy.sh root@服务器IP          # 默认部署到 /opt/xlh
XLH_STATE_DIR=/srv/xlh-state WITH_PUSH=1 scripts/deploy.sh root@服务器IP
```

**`WITH_PUSH=1` 别漏** —— `xlh-push` 在 compose 里是可选 profile，不带它时 `up -d` **完全不管这个服务**（不建、不重建、看都不看）。而**盘中实时抓取整个挂在这个守护上**（`push/schedule.rs` 的 60s 循环），它不跑就永远没有实时数据，偏偏 `xlh-web` 一切正常、页面照开，从外面看不出问题。查 `xlh.db` 的 `push_heartbeat` 表：0 行 = 守护从没跑过。

**打包给老版本 Docker**：buildx 默认产出 OCI image index + attestation manifest，老 Docker（20.10 等）`docker load` 会失败或加载出跑不起来的镜像。服务器 Docker 版本不确定时：

```bash
docker build --provenance=false --sbom=false --output type=docker,name=xlh:latest .
```

这样本地镜像存储里就是单一 `manifest.v2+json`，`deploy.sh` 自己那步 `docker save` 导出的也干净。

手动路径（本机 **PowerShell** 用 `docker save -o`，别用 `| gzip >`，PowerShell 无 gzip 且 `>` 会损坏二进制）：

```powershell
docker save xlh:latest -o xlh-latest.tar
scp xlh-latest.tar docker-compose.prod.yml config.toml user@服务器IP:/opt/xlh/
```

```bash
# 服务器（Linux）
cd /opt/xlh

# 1. .env 必须先有，且 XLH_STATE_DIR 必须是绝对路径。
#    compose 里它是 `:?` 强制的 —— 没有就直接报错起不来。这是故意的：
#    静默回退到空库会让服务照常启动、所有账号却登不上，看起来像「数据丢了」。
cat > .env <<'EOF'
XLH_STATE_DIR=/opt/xlh
XLH_IMAGE=xlh:latest
XLH_BIND_ADDR=127.0.0.1
XLH_PORT=8080
TZ=Asia/Shanghai
EOF

# 2. 先备份。用 .backup 而非 cp —— 库跑在 WAL 模式，数据可能几乎全在 -wal 里，
#    cp 主库会得到一个能打开、看着正常、其实没数据的空壳。
mkdir -p "$(. ./.env; echo $XLH_STATE_DIR)"/{data,cache,output,backups}
sqlite3 /opt/xlh/data/xlh.db ".backup '/opt/xlh/backups/xlh-$(date +%F-%H%M%S).db'" 2>/dev/null || true

# 3. 加载 + 起。--profile push 每条命令都要带，漏了 xlh-push 就不会被创建。
docker load -i xlh-latest.tar
docker compose -f docker-compose.prod.yml --profile push up -d --force-recreate --remove-orphans
docker compose -f docker-compose.prod.yml --profile push ps
```

**部署后必须验证**（容器 `Up` 不等于活着，崩溃循环在 `ps` 里也占一行）：

```bash
docker logs --tail 20 xlh-push     # 要看到「实时抓取已启用（库 data/realtime.db，ticks 保留 10 天）」
docker ps -a --filter name=xlh- --format '{{.Names}} {{.Status}}' | grep -Ei 'restarting|exited'
```

盘中每 10 分钟还应有一行 `[HH:MM] 快照 N 条，异动 N 只`。只有推送日志、没有「实时抓取已启用」= `config.toml` 的 `[realtime]` 段没被读到。

采集数据默认永久保留：`[realtime] retain_days = 0`，不自动删除历史 ticks；signals 也永久保留。`baseline_days = 10` 只控制量能统计窗口，不影响数据保存。只有显式设置正数 `retain_days` 才启用过期清理。升级已有部署时，请同步更新配置并重新构建、重启服务；旧版程序不支持值 0。数据库持续增长，请定期备份。

**对外暴露**：`docker-compose.prod.yml` 默认把端口绑在 `127.0.0.1:8080`（防裸奔）。要开外网访问，改成 `- "8080:8080"` 后 `up -d`，**并在云安全组/防火墙放行入方向 TCP 8080**（来源建议限成你自己的 IP）。因界面无鉴权且 `push.toml` 含密钥，长期对外强烈建议前置 Nginx/Caddy + Basic Auth + HTTPS，只对外开 443。

**常见坑**

- **实时异动一直没数据、`ticks` 表空**：`xlh-push` 没在跑。实时抓取挂在推送守护的 60s 循环上，不在 `serve` 里 —— `serve` 只调 `realtime::config::init` 供 Web 读榜，从不启动抓取。用 `--profile push` 起 `xlh-push`；`push_heartbeat` 为 0 行即可确认守护从没跑过。另注：`baseline_days = 10`，冷启动前 10 个交易日 `Baseline` 只会是 `Fallback`（拿当日自己比自己），不是 `History`。
- **`docker load` 报 invalid/unsupported manifest，或加载后跑不起来**：包是 buildx 的 OCI index + attestation，服务器 Docker 太老吃不下。用 `--provenance=false --sbom=false --output type=docker` 重新构建再 save。验包：`tar -xzOf xlh-latest.tar.gz index.json` 应只有 `application/vnd.docker.distribution.manifest.v2+json`。
- **`error: XLH_STATE_DIR is required` / compose 直接起不来**：服务器上 `.env` 不存在或没设 `XLH_STATE_DIR`（必须绝对路径）。这是刻意设计的强制失败，别去掉 `:?` 加默认值 —— 静默回退到空库比起不来危险得多。`deploy.sh` 会自动生成 `.env`，手工部署要自己写。
- **`写入 push.toml 失败: Is a directory`**：启动容器时宿主 `push.toml` **文件不存在**，Docker 会把挂载源自动建成**目录**。修复：`docker compose ... down` → `rm -rf push.toml` → 重新 `scp` 真正的文件 → 确认 `ls -l push.toml` 是文件 → `up -d`。**务必先放好文件再 `up`**（`config.toml` 同理）。
- **外网打不开、`docker ps` 显示 `127.0.0.1:8080->8080`**：端口只绑了本机，按上面「对外暴露」改绑定并放行安全组。
- **Windows Docker Desktop 本机 `localhost:8080` 返回 502**：是 Docker Desktop 的 WSL2 端口转发问题（对所有容器都复现，与本项目无关），Linux 服务器上不存在。

### 授权与收费

本项目内置 **SaaS 授权体系**，支持在线激活、到期宽限与功能锁定。

**首次部署：创建管理员账户**

```bash
XLH_ADMIN_PASSWORD=YourSecurePassword123 xlh admin create --username admin

docker exec -e XLH_ADMIN_PASSWORD='换成你自己的强密码' xlh-web xlh admin create --username admin
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

## 量化交易（一期）

工单闭环 + 模拟盘 + 策略准入，把系统信号/策略变成可跟踪结果的交易工单。**不对接任何券商接口，不自动下单**——所有实盘操作由用户在自己的券商 App 手动完成，成交后回到 `/trade` 页面回填实际成交价与数量，系统据此更新持仓并继续监听止盈止损。本模块及其信号/成绩单**不构成投资建议**。

### 运行方式

交易相关的两个后台线程随 **`xlh push`**（守护模式，非 `--once`）启动，与推送/实时异动同一进程，不是独立命令。`[trade]` 段缺失时按默认值启动；`config.toml` 读取/解析失败或 `[trade]` 段（含其子段）校验不通过时，打印警告且两个线程都不启动；`[trade] enabled = false` 同样两个都不启动：

- **`trade-monitor`**：止盈止损秒级监听（每 `monitor_interval_secs` 秒一轮）、次日 9:00 撤销未回填工单、15:05–16:00 提醒待回填、日线策略信号发出、收盘后交易日报
- **`trade-eval`**（另需 `[trade.eval].enabled = true`）：策略前推回测排队执行、每日观察期检查/实盘看门狗、每月重跑

`xlh serve` 提供 `/trade` 页面与 `/api/trade/*` 接口；`serve` 本身不启动上述两个线程，盘中数据要靠 `xlh push` 常驻才会更新（同「实时异动」的坑）。两者共用同一个 SQLite 库（`data/xlh.db`），所有查询按 `user_id` 隔离。

### 配置

`config.toml` 中（段缺失则用默认值；键名与校验以 `src/trade/config.rs` 为准，文件内已附注释样例）：

| 段 | 用途 | 常用项 |
|---|---|---|
| `[trade]` | 监听线程总开关与推送相关参数 | `enabled`（总开关，同时决定 `trade-monitor` 是否启动）、`monitor_interval_secs`（报价轮询间隔，默认 15 秒）、`alert_after_secs`（监听中断多久告警）、`mover_signals`（实时异动是否转为交易信号）、`link_base_url`（推送工单签名链接的站点根地址，留空则不带链接）、`daily_report`/`daily_report_hour`/`daily_report_minute`（收盘后交易日报） |
| `[trade.admission]` | 策略准入与持续监控阈值（夏普、回撤、观察期天数等）；由管理员在配置文件中统一设置，暂不支持按用户调整 | 默认值见 `config.rs`（`config.toml` 未附注释样例） |
| `[trade.walk_forward]` | 前推回测窗口切分（训练/检验/步长）与选参依据 | 默认训练 2 年、检验 6 个月、步长 6 个月 |
| `[trade.eval]` | 策略评估线程 `trade-eval` | `enabled`、`poll_secs`、`daily_hour`/`daily_minute`（每日入队观察期检查/看门狗）、`monthly_day`/`monthly_hour`（每月重跑前推回测） |
| `[trade.signals]` | 观察期/已准入策略的日线信号：收盘后计算、次日开盘发出 | `enabled`、`compute_hour`/`compute_minute`（开始计算）、`emit_hour`/`emit_minute` ~ `emit_end_hour`/`emit_end_minute`（次日发出窗口，固定不晚于 10:30） |

### 页面

`/trade` 七个标签：

- **待确认**：新工单，含建议价/现价/偏离、预估金额与费用、理由与 AI 说明；行情延迟（现价超 60 秒未更新）禁止确认，偏离过大需二次点击确认
- **待成交**：已确认、等待回填的实盘工单；当日未回填次日 9:00 自动撤销
- **已完成**：最近 100 张终态工单（已成交 / 已过期 / 已拒绝 / 已取消）
- **被拦截的信号**：闸门（去重、冷却、准入、风控等）拒绝的信号及原因
- **策略**：新建/编辑策略、提交前推回测、查看成绩单与状态变更记录、取消评估任务
- **风控设置**：单笔/单票上限、每日工单上限、冷却、偏离阈值、默认止盈止损、模拟盘滑点、账户资金
- **持仓校准**：以券商账户为准修正系统记录的持仓（仅实盘）

首页个股诊断 / AI 分析结果旁有「生成工单」入口，按当前分析一键生成手动工单（与自动信号同一闸门：仅交易时段、需当日新鲜行情，重复提交幂等）。

推送里的工单附带签名链接（`/trade/t/:id?sig=...`），免登录即可查看与确认该工单；签名绑定工单号/用户/过期时刻，三者任一变化即失效，**有效期至该工单自身过期为止**（止盈止损 30 分钟、日线策略次日至多到 10:30、异动 10 分钟、手动工单当日至多到 15:00）。需配置 `link_base_url` 为对外可访问地址，否则推送不带链接。

### 信号源与准入

四类信号源：止盈止损（`exit`，持仓监听触发）、日线策略（`strategy`，收盘后计算、次日开盘发出）、实时异动（`mover`，自选股及观察期/已准入异动策略股票池中的股票）、手动 / AI（`manual`，页面按诊断/分析结果生成）。`exit` 与 `manual` 不受策略准入约束；未绑定异动策略的 `mover` 信号（仅来自自选股）一律只进模拟盘；`strategy` 信号与绑定了异动策略的 `mover` 信号按来源策略的准入状态出单：新建策略是**草稿**，提交评估后进入**回测中**（前推回测，数据不足或指标不达标 → **未通过**）；通过后进入**观察期**（仅模拟盘自动成交，需满足观察天数/笔数与表现要求）；达标后**已准入**（实盘 + 模拟盘同时生成工单）；实盘期间持续监控，回撤、胜率、连亏异常触发 → **已暂停**，需用户手动重新提交前推回测才能再次进入观察期。

### 管理员总开关

`/admin` 后台「交易总开关」区块（接口 `/api/admin/trade/kill-switch`）：打开后所有用户的新信号一律被闸门拒绝，待确认工单也无法再确认；已确认、等待回填的工单不受影响，可正常回填成交。操作人与操作时间留痕在开关状态里。

### 常见问题

- **工单确认不了**：行情延迟（现价超过 60 秒未更新）、价格偏离建议价过多（按钮会变成「价格已偏离，仍要确认」，需二次点击）、或管理员已打开总开关暂停交易
- **日线信号没发出**：来源策略不在观察期 / 已准入状态（草稿、回测中、未通过或已暂停都不会发）、策略缺实盘参数（准入机制上线前就在观察期的策略需先补跑一次前推回测，`trade-eval` 每次进程重启后会自动补排一次）、或当日 K 线还没更新（按 `retry_minutes` 重试，证实开市当日到指定整点仍无 K 线则按停牌记「无操作」，不再重试）
- **签名链接打不开**：`link_base_url` 未配置（推送时就不会带链接）、或工单已过期 / 已处理（链接与工单同一生命周期，不单独续期）

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
| `/trade` | GET | 交易页（需登录）：待确认/待成交/已完成/被拦截的信号/策略/风控设置/持仓校准 七个标签 |
| `/trade/t/:id` | GET | 工单签名链接落地页（免登录，`?sig=` 校验） |
| `/api/trade/*` | GET/POST | 交易 API（需登录+授权，另见「量化交易（一期）」），按功能分组：工单（列表/确认/忽略/回填/手动生成）、被拦信号、风控与资金（风控设置/账户资金）、持仓（列表/止盈止损/校准/校准记录）、策略（增删改/提交评估/成绩单/状态变更）、评估任务（列表/取消）、概览（心跳/账户/总开关） |

### 技术栈

Rust 2021 · axum 0.7 · tokio · reqwest（rustls）· plotters · clap · serde/serde_json · toml · chrono · anyhow/thiserror。
