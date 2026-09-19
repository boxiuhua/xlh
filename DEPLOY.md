# 部署

## TL;DR

```bash
# 本机
docker build -t xlh:latest .
scripts/deploy.sh user@your-server            # 仅 Web
WITH_PUSH=1 scripts/deploy.sh user@your-server # 同时起定时推送守护
```

数据在服务器的 **`/opt/xlh/data/xlh.db`**。

`deploy.sh` 只 scp 具体文件进部署目录（镜像包、compose、config），**从不 `rm -rf`、
从不整目录覆盖** —— 所以重新部署不会丢数据，每次部署前还会自动备份一份。

想彻底免疫手工误删（`rm -rf /opt/xlh`、`rsync --delete`、换别的部署工具），
把状态目录指到部署目录之外：

```bash
XLH_STATE_DIR=/srv/xlh-state scripts/deploy.sh user@your-server
```

---

## 数据在哪，为什么这么放

`XLH_STATE_DIR` 默认 `/opt/xlh`（即部署目录）。

| 路径 | 内容 | 丢了会怎样 |
|---|---|---|
| `/opt/xlh/data/xlh.db` | 用户、授权码、会话、推送配置、建议历史 | **不可再生。全没了。** |
| `/opt/xlh/cache/` | 净值 / K线 / 财报 / 估值缓存 | 可再生，但全量重抓要很久 |
| `/opt/xlh/output/` | 回测报告 HTML | 无所谓 |
| `/opt/xlh/backups/` | 自动备份（每次部署前一份，永久保留） | — |

### 放在部署目录里安全吗

`deploy.sh` **只 scp 具体文件**进部署目录（`xlh-latest.tar.gz`、`docker-compose.prod.yml`、
`config.toml`），从不 `rm -rf`、从不整目录覆盖。所以**它自己不会弄丢数据**。

风险在脚本之外：手工 `rm -rf /opt/xlh`、`rsync --delete` 同步、或换用别的部署工具
（ansible copy 之类），都会连数据一起抹掉。要免疫这类事故，把状态指到部署目录之外：

```bash
XLH_STATE_DIR=/srv/xlh-state scripts/deploy.sh user@your-server
```

`deploy.sh` 会自动检测旧位置的数据并询问是否迁移。

### 一个刻意的设计

`docker-compose.prod.yml` **不给 `XLH_STATE_DIR` 设默认值** —— 没设就直接报错启动失败。

静默回退（比如悄悄用 `./data`）是最危险的失败方式：服务照常起来、页面照常打开，
但读的是一个空库，**所有账号都登不上**，看起来像"数据丢了"。宁可起不来，也不要这样。

---

## 备份：为什么不能只 `cp xlh.db`

数据库跑在 **WAL 模式**下。实测一个刚建好用户的库：

```
xlh.db      4,096 字节   ← 主库几乎是空的
xlh.db-wal 78,312 字节   ← 数据实际在这里
```

只 `cp xlh.db` 得到的备份：

```
$ sqlite3 bad-backup.db "select count(*) from users;"
Error: in prepare, no such table: users
```

**连表都没有。** 但它能打开、文件名正确、大小合理 —— 这种「看似成功的备份」比没有备份更危险，
因为你会以为自己有退路。

正确做法（`scripts/backup.sh` 已经这么做）：

```bash
sqlite3 xlh.db ".backup out.db"    # 会把 WAL checkpoint 进去
```

没有 sqlite3 时，`cp` 必须连 `-wal` / `-shm` 一起拷。

### 用法

```bash
scripts/backup.sh user@host              # 备份并拉回本机 ./backups/
scripts/backup.sh --list user@host       # 看服务器上有哪些备份
scripts/backup.sh --restore user@host xlh-20260712-1930.db
```

`deploy.sh` 每次部署前会自动备份一次（永久保留），并在部署后**比对用户数** ——
数据少了会立刻报错退出，而不是等你哪天登录时才发现。

建议再加一条 crontab：

```cron
0 3 * * * cd /opt/xlh && XLH_STATE_DIR=/srv/xlh-state /opt/xlh/backup.sh
```

> 备份和数据放在同一台机器上，机器没了就一起没了。`backup.sh` 在指定 `user@host` 时会把备份
> 拉回本机 —— 至少让它们不在同一块盘上。

---

## 配置

`.env`（服务器上 `/opt/xlh/.env`，首次部署自动生成，之后不会被覆盖）：

```ini
XLH_STATE_DIR=/srv/xlh-state   # 绝对路径，必须在部署目录之外
XLH_BIND_ADDR=127.0.0.1        # 只绑回环，前面套 Nginx/Caddy
XLH_PORT=8080
TZ=Asia/Shanghai               # cron 按此时区解释，错了定时推送会差 8 小时
XLH_IMAGE=xlh:latest
```

Web 默认只监听 `127.0.0.1:8080`。**不要直接对公网开放** —— 前面配 Nginx/Caddy 做 HTTPS 反代。

---

## 推送守护

```bash
WITH_PUSH=1 scripts/deploy.sh user@host
```

它和 Web 共用同一个 `xlh.db`：从 `push_configs` 表读各用户配置、写 `advice_history`。
守护进程**每 60 秒重读一次配置**（`schedule.rs` 的循环里 `store::list_all`），所以在 Web 上
改完推送配置**不需要重启容器**。

`push.toml` 是遗留文件，只在首次启动时把旧的全局配置一次性导入数据库。之后它不再是存储位置，
删了也不影响。

---

## 交易模块上线检查清单

生产环境要让止盈止损监听、日线策略信号、交易日报正常工作，逐项确认：

- **`xlh push` 必须常驻**：止损监听（`trade-monitor`）与策略评估（`trade-eval`）都挂在 `push`
  守护进程里，不在 `serve` 里，同「推送守护」一节；`docker-compose.prod.yml` 要用
  `--profile push` 起 `xlh-push`，漏了这个 profile 交易模块完全不跑，`xlh-web` 页面却照常打开、
  从外面看不出问题。用 `docker logs xlh-push` 或 `/trade` 页顶部「监听 / 评估」心跳确认。
- **`link_base_url` 设为对外可访问的 HTTPS 地址**（`config.toml` 的 `[trade]` 段）：不填的话推送里
  的工单不带签名链接，用户只能登录网页确认，收不到能直接点开的链接。
- **反向代理不要覆盖 `Referrer-Policy` 头**：工单签名链接落地页自己设了
  `Referrer-Policy: no-referrer`，防止签名经 Referer 泄露给第三方；Nginx/Caddy 若统一加了这个
  响应头，确认没有对 `/trade/t/*` 单独覆盖成更宽松的值（比如 `same-origin` 或不设置）。
- **`data/xlh.db` 备份含交易表**：交易工单、持仓、策略定义、评估任务都存在同一个 `xlh.db` 里，
  跟用户/授权表一起走上面「备份」一节的 `sqlite3 ... ".backup"` 流程即可，不需要单独处理，
  但升级/迁移时同样不能漏了这个库（否则丢的不只是账号，连持仓与策略状态一并丢失）。
- **时区必须是 `Asia/Shanghai`**（`.env` 的 `TZ`）：`trade-monitor`/`trade-eval` 按容器本地时间
  判断交易时段、收盘计算、次日发出窗口、日报窗口等各类到点任务；时区配错不会报错，只会让这些
  任务整体错开几小时，现象和「没生效」一样不好排查。

---

## 从旧版本升级（数据在部署目录里的那种）

旧版把数据放在部署目录内（`/opt/xlh/data`）。新版挪到了 `$XLH_STATE_DIR`（`/srv/xlh-state`）。

**不迁移的话，服务能正常起来，但所有账号都登不上** —— 因为新目录是空的，容器建了个全新的空库。
数据没丢，只是没人去读它。这种「看起来是数据丢了、其实是读错了地方」的失败最难排查。

`scripts/deploy.sh` 会自动检测并询问是否迁移。手工迁移：

```bash
# 服务器上
cd /opt/xlh
docker compose -f docker-compose.prod.yml down

mkdir -p /srv/xlh-state/{data,cache,output,backups}

# 必须用 .backup，不能 cp —— 库在 WAL 模式下，数据可能几乎全在 -wal 里
sqlite3 /opt/xlh/data/xlh.db ".backup '/srv/xlh-state/data/xlh.db'"

# 缓存可再生，但重抓很慢，一并搬
cp -r /opt/xlh/.cache/. /srv/xlh-state/cache/

# 校验：用户都在吗？
sqlite3 /srv/xlh-state/data/xlh.db "select id,username from users;"

# 旧目录改名而非删除，留条回头路
mv /opt/xlh/data /opt/xlh/data.migrated

docker compose -f docker-compose.prod.yml up -d
```

---

## 坑

**别同时用原生二进制和容器跑同一个 data 目录。** SQLite 的 WAL 需要共享内存段（`-shm`），
两个进程隔着 Windows/macOS 的 bind mount 抢同一个库会失败：

```
Error code 4618: I/O error within the xShmMap method
```

容器会陷入崩溃重启循环。要么停掉原生进程，要么让它们用不同的 data 目录。

**首次部署后立刻建管理员：**

```bash
ssh user@host "cd /opt/xlh && docker compose -f docker-compose.prod.yml exec \
  -e XLH_ADMIN_PASSWORD='强密码' xlh-web xlh admin create --username admin"
```
