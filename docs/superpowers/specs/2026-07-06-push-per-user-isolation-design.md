# 推送按用户隔离（多租户化）设计

日期：2026-07-06
状态：已批准，待实现

## 背景

xlh 已转为收费 SaaS（账号 + 授权码激活，SQLite，会话 cookie）。当前推送是
**单租户**：

- 全局单文件 `push.toml` 存**一份** `PushConfig`（cron 排期 + 渠道 webhook/secret +
  基金/股票持仓 + 诊断项），见 `src/push/config.rs`。
- Web 路由 `/api/push/{config,preview,test}` 挂在**管理员组**（`require_admin`），
  因原本存的是运营者密钥（`src/web/mod.rs::push_routes`）。
- 守护进程 `xlh push [--file push.toml] [--once]` 按**那一份** cron 循环，投递到
  **那一个**渠道（`src/push/schedule.rs::run_daemon`、`src/push/job.rs`）。
- `xlh serve`（Web）与 `xlh push`（守护）是**两个进程**，但都打开**同一** SQLite
  主库（`data/xlh.db`，见 `src/main.rs`）。

目标：把推送改为**每用户隔离**——每个用户有自己的持仓 + 自己的渠道 + 自己的排期，
定时投递到各自渠道。

## 目标与非目标

**目标**
- 每个「登录 + 授权」用户在主界面「推送」Tab 管理**自己的** `PushConfig`（增删改查、
  预览、测试发送）。
- 守护进程遍历所有用户配置，按各自 cron 定时投递到各自渠道，历史按 user_id 记录。
- 旧全局 `push.toml` 启动时导入首个管理员账号，随后弃用该文件。
- 服务端加固：忽略用户提交的 `cache_dir`；拒绝会「每秒触发」的 cron。

**非目标**
- 每用户推送历史查看页（历史按 user_id 落库，但仅管理员 `/api/admin/push-history`
  审计全量，不新增用户侧历史 UI）。
- 守护进程并行化（串行足够；数据缓存全局共享）。
- 运营者「全局广播」推送（本期纯每用户自管，管理员不再持有全局推送配置）。
- cron 频率的精细限流（仅做「秒位必须固定」这一条防狂刷）。

## 存储：`src/push/store.rs`（新模块）

主库新表（随迁移建立）：

```sql
CREATE TABLE IF NOT EXISTS push_configs (
  user_id     INTEGER PRIMARY KEY REFERENCES users(id),
  config_json TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
```

`PushConfig` 以 `serde_json` 序列化存入 `config_json`。函数：

| 函数 | 语义 |
|---|---|
| `migrate(conn) -> Result<()>` | 建表（`CREATE TABLE IF NOT EXISTS`） |
| `upsert(conn, user_id, cfg: &PushConfig) -> Result<()>` | `INSERT … ON CONFLICT(user_id) DO UPDATE`，写 `config_json` + `updated_at` |
| `get(conn, user_id) -> Result<Option<PushConfig>>` | 读并反序列化；行不存在或 JSON 损坏→`None`（fail-soft） |
| `delete(conn, user_id) -> Result<()>` | `DELETE FROM push_configs WHERE user_id = ?1` |
| `list_active(conn) -> Result<Vec<(i64, PushConfig)>>` | `push_configs pc JOIN users u ON u.id = pc.user_id`，返回 `(user_id, cfg)`；JOIN 天然排除已删用户的孤儿行；损坏 JSON 行跳过 |

`migrate` 在 Web 启动（`web::serve`）与守护启动（`xlh push`）时都会调用，确保表存在。

## Web：路由从「管理员组」迁到「登录 + 授权组」，按当前用户隔离

`src/web/mod.rs`：

- 删除 admin 组里的 `push_routes` 合并；改为在 **licensed 组**（`require_license` +
  `require_login`）挂载 per-user 推送路由。
- 处理器由泛型 `S` 改为具体 `State<AuthState> + Extension<CurrentUser>`：

  - `GET /api/push/config`：`store::get(conn, user.id)`；无行→`default_config()`。
  - `POST /api/push/config`：
    1. `config::validate(&cfg)`（渠道/webhook/cron/目标非空）→ 失败 400。
    2. **加固** `harden(&mut cfg)`：`cfg.channel.cache_dir = default_cache_dir()`
       （忽略客户端值）。
    3. **cron 秒位固定校验** `require_fixed_seconds(&cfg.schedule.cron)`：秒字段为
       `*`、范围或列表 → 400（防每秒狂刷）。
    4. `store::upsert(conn, user.id, &cfg)`。
  - `POST /api/push/preview`：从请求体取 cfg，`harden`，`build_message`；无状态。
  - `POST /api/push/test`：从请求体取 cfg，`validate` + `harden` + 秒位校验，
    `run_forced(&cfg, hist, Some(user.id))` 发到该用户自填 webhook 并落历史。

主 SPA 已含「推送」Tab（`collectPushConfig`、`/api/push/config`、`pu-test`），
本改造顺带让每个授权用户的推送 Tab 真正可用（此前被 admin 门禁挡住）。前端预期无改动；
若发现 Tab 依赖 admin 才显示，则移除该门禁（实现时确认）。

**测试改造**：现有 `push_config_get_returns_json` / `push_config_save_empty_webhook_is_400`
走 `core_router`/`post_json`（无鉴权）。迁移后这些端点需鉴权，测试改为 seed 一个
「已授权」用户 + cookie 走生产 `router`。

## 多租户定时守护：`src/push/schedule.rs` + 编排

`xlh push` 命令（`src/main.rs`）改为：打开主 DB → `store::migrate` →
`migrate_legacy_push` → 跑循环。**不再需要 `--file`**。

多租户 tick 循环：

- 启动：`last_tick = Local::now()`（不补历史触发）。
- 每次迭代：sleep 到下一分钟对齐点；`now = Local::now()`。
- `for (uid, cfg) in store::list_active(conn)`：
  - 该用户授权须放行：`find_user_by_id` → `!disabled && !cancelled` 且
    `LicenseStatus::of(expires_at, now, warn, grace).allows_access()`；否则跳过。
  - cron 命中判定：`next_after(&cfg.schedule.cron, &last_tick)? <= now` → 到点。
  - 到点则 `run_scheduled(&cfg, conn, uid)`：`build_message_full` →
    `only_on_new_data && !has_new` 跳过 → `channels::send` →
    `history::save(conn, Some(uid), "push", …)`。单次失败仅记日志、继续下一个用户。
- 迭代末：`last_tick = now`。

`warn_days`/`grace_days` 从 `load_auth(config)` 取（与 Web 同源）。

`--once`：对所有 `list_active` 用户强制跑一次（忽略 `only_on_new_data`），用于手动全量触发。

## job 层调整：`src/push/job.rs`、`src/push/mod.rs`

- `save_push_history` 增加 `user_id: Option<i64>` 参数，透传给 `history::save`
  （现为写死 `None`）。
- `run(cfg, hist, user_id)` / `run_forced(cfg, hist, user_id)` 增加 `user_id`。
- `mod.rs`：`run_once(cfg, hist, user_id)`；移除单文件 `run_daemon(cfg, hist)`，
  由 `schedule` 内的多租户循环取代（新增 `run_multi_daemon(conn, cfg_auth)`）。

## 旧 `push.toml` 迁移

`migrate_legacy_push(conn, path) -> Result<()>`（守护与 Web 启动均调用一次，幂等）：

- 若 `path` 存在且 `push::config::load(path)` 成功：
- 取首个管理员 `first_admin_id(conn)`（`SELECT id FROM users WHERE is_admin=1
  ORDER BY id LIMIT 1`，需新增 store 辅助）。
- 若该管理员**尚无** `push_configs` 行（`store::get` 为 `None`）→ `store::upsert`
  导入。已有行则不覆盖。
- 无管理员 / 无文件 / 解析失败 → 静默跳过（记日志）。

## 加固细节

- `harden(cfg)`：`cfg.channel.cache_dir = default_cache_dir()`。用户不可指定缓存路径。
- `require_fixed_seconds(cron)`：拆分 cron 首字段（秒），必须是纯数字（如 `0`、`30`）；
  含 `*` / `,` / `-` / `/` → 拒绝。保证每分钟最多触发一次量级，杜绝每秒任务。
  该校验仅用于 Web 写入路径；CLI/既有 `validate` 不变（现有测试样本秒位均为固定值）。

## 错误码（Web）

沿用 `anyhow` → 现有错误响应风格（`push_config_save` 等目前返回 `Result<_, AppError>`，
校验失败转 400）。新增校验失败信息：
- `channel.webhook 不能为空` 等（validate 既有）。
- `cron 秒位必须为固定值（不支持 * 或范围），以避免过于频繁的推送`。

## 跨特性依赖与顺序

`store::delete_user`（账户管理特性）在删号时应顺带 `push_store::delete(conn, uid)`
清理该用户推送配置。因 `list_active` 用 JOIN 已容忍孤儿行，此清理为**整洁性优化、
非硬阻塞**。建议**先实施账户管理，再实施本推送改造**；实现本特性时在 `delete_user`
事务中补一条 `DELETE FROM push_configs WHERE user_id = ?1`。

## 测试

**`push::store`（单测）**
- `upsert` 后 `get` 往返得到等价 config；重复 `upsert` 覆盖。
- `list_active`：含配置的用户返回；无 `push_configs` 行的用户不返回；`push_configs`
  指向已删用户（无对应 users 行）不返回（JOIN 排除）。
- `delete` 后 `get` 为 `None`。
- `get` 遇损坏 JSON 返回 `None`（不 panic）。

**Web（licensed 组，seed 已授权用户 + cookie 走生产 `router`）**
- `GET /api/push/config` 无行 → 200 且返回默认 config JSON。
- `POST /api/push/config` 空 webhook → 400。
- `POST /api/push/config` cron 秒位为 `*`（如 `* 30 8 * * *`）→ 400。
- `POST /api/push/config` 提交自定义 `cache_dir` → 保存后 `get` 显示被覆盖为默认。
- `POST` 合法 → 200，随后 `GET` 读回一致。
- 未登录 → 401；已登录未授权 → 403。

**调度（`schedule`）**
- `next_after` 窗口判定：`last_tick` 之后、`now` 之前有触发 → 命中；无触发 → 不命中。
- 授权无效用户被跳过（构造 expired / cancelled 用户，断言其不产生投递——可用可注入的
  「发送器」或对 `run_scheduled` 的可测拆分；实现时以纯函数 `due_users(configs, now,
  last_tick)` 返回应触发的 uid 列表来单测，避免真实网络）。

**迁移**
- `migrate_legacy_push`：首个管理员无行 → 导入；已有行 → 不覆盖；无管理员 → 跳过。

## 影响文件

- `src/push/store.rs`（新：表 + CRUD + `list_active` + 单测）
- `src/push/schedule.rs`（多租户循环 + `due_users` 纯函数 + 测试）
- `src/push/job.rs`（`user_id` 落历史；`run`/`run_forced` 签名）
- `src/push/mod.rs`（`run_once`/守护入口签名）
- `src/push/config.rs`（`harden`、`require_fixed_seconds`、`default_cache_dir` 复用）
- `src/web/mod.rs`（路由迁移到 licensed 组 + per-user 处理器 + 测试改造）
- `src/main.rs`（`push` 命令读 DB + `migrate_legacy_push`；移除 `--file` 依赖）
- `src/web/auth/store.rs`（`delete_user` 顺带清 `push_configs`——整洁性，依赖账户管理）
