# xlh SaaS 授权收费体系 —— 设计文档

- 日期：2026-07-05
- 状态：已确认，待实现
- 关联记忆：[[stock-system-plan]]、[[user-autonomy-preference]]

## 1. 背景与目标

xlh 现为单 crate 的基金/股票分析工具，通过 `xlh serve`（axum）对外提供 Web 界面，
统一托管在作者的服务器上。现要把它变成**收费产品**：客户注册账号、凭**授权码**激活后
方可使用核心分析能力；到期后被锁，续期需再激活。

**交付模式**：SaaS（作者统一托管，客户浏览器访问）。**不做**私有部署，因此无需绑机器、
防拷贝等离线 License 机制——服务端完全受控。

**收款**：v1 手动发码（客户线下付款 → 作者在管理后台生成授权码发给客户）。在线支付不在本期范围。

## 2. 核心决策（已与用户确认）

| 维度 | 决策 |
|---|---|
| 交付 | SaaS 托管，非私有部署 |
| 授权形式 | 账号 + 授权码激活 |
| 注册 | 开放自助注册；未激活时核心功能锁定 |
| 授权码 | 只带「时长」，不带功能档位（v1 不做分版本） |
| 收款 | 手动发码 |
| 存储 | SQLite（路线 A） |
| 运维 | 网页管理后台（首个管理员用 CLI 引导） |
| 失效行为 | 到期前提醒 + 宽限期 + 到期后锁 API |

## 3. 授权状态机

单一事实来源：`user.expires_at: Option<Date>`。配置项 `warn_days`（默认 7）、
`grace_days`（默认 3）。给定「当前日期 now」，计算有效状态 `LicenseStatus`：

| 条件 | 状态 | 核心 API 放行? | 前端顶栏 |
|---|---|---|---|
| `expires_at = None` | `Inactive`（未激活） | ❌ | 「请激活授权码」 |
| `now ≤ expires_at − warn_days` | `Active`（正常） | ✅ | 绿色，显示到期日 |
| `expires_at − warn_days < now ≤ expires_at` | `Warning`（临期） | ✅ | 黄色「N 天后到期，请续期」 |
| `expires_at < now ≤ expires_at + grace_days` | `Grace`（宽限期） | ✅ | 红色「已到期，宽限剩 M 天」 |
| `now > expires_at + grace_days` | `Expired`（已过期） | ❌ | 「已过期，请续期」 |

- 中间件放行集合：`{Active, Warning, Grace}`；拦截集合：`{Inactive, Expired}`。
- 另有账号级 `disabled` 布尔（封禁），封禁用户一律拦截（视同 `Inactive`，登录也拒绝）。
- 状态由纯函数 `LicenseStatus::of(expires_at, now, warn_days, grace_days)` 计算，可单测。

**激活语义**：激活一张 `days` 天的授权码时
`expires_at = max(now, 原 expires_at.unwrap_or(now)) + days`，
即续期从「now 与原到期日的较大者」起算（未过期续期不损失剩余天数，已过期续期从今天起算）。
同一事务内把该码标记 `used_by = user_id, used_at = now`。授权码**一次性**，由 DB 唯一约束 +
「仅当 `used_by IS NULL` 时才允许 UPDATE」的条件更新保证并发下不被重复使用。

## 4. 架构与模块

Web 是组合根，允许自由 `use` 任何模块（隔离约束只限 `stock` 不 use 基金业务模块，与本功能无关）。

```
src/web/auth/
  mod.rs         组装：暴露 auth_routes()/admin_routes()/auth_gate/admin_gate/AuthState
  store.rs       SQLite 封装：连接、建表迁移，users/codes/sessions 的 CRUD
  model.rs       User / Code / Session 结构体 + LicenseStatus 状态机（纯函数）
  password.rs    argon2 密码哈希与校验
  session.rs     会话 token 生成（rand）、Cookie 读写（HttpOnly/SameSite/Secure）
  handlers.rs    /api/auth/{register,login,logout,activate,me}
  admin.rs       /api/admin/*（发码/列码/作废/用户管理/概览）+ /admin 页面渲染
  middleware.rs  auth_gate（拦核心 /api/*）+ admin_gate（拦 /admin 与 /api/admin/*）
src/web/page_auth.rs   登录/注册/激活页 HTML + 管理后台页 HTML
```

`AuthState`（存 SQLite 连接池句柄 + 配置）通过 axum `State` / `Extension` 注入 handler 与中间件。

### 4.1 数据表（SQLite）

```sql
CREATE TABLE users (
  id            INTEGER PRIMARY KEY,
  username      TEXT NOT NULL UNIQUE,
  pw_hash       TEXT NOT NULL,          -- argon2 PHC 字符串（含盐）
  expires_at    TEXT,                   -- ISO date，NULL = 未激活
  is_admin      INTEGER NOT NULL DEFAULT 0,
  disabled      INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL
);
CREATE TABLE codes (
  code          TEXT PRIMARY KEY,       -- 随机不易猜的码串
  days          INTEGER NOT NULL,
  used_by       INTEGER REFERENCES users(id),
  used_at       TEXT,
  revoked       INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL
);
CREATE TABLE sessions (
  token         TEXT PRIMARY KEY,       -- 随机 token
  user_id       INTEGER NOT NULL REFERENCES users(id),
  expires_at    TEXT NOT NULL,          -- 会话过期（如 30 天）
  created_at    TEXT NOT NULL
);
```

并发写：SQLite 开启 WAL 模式；连接以互斥方式访问（`Mutex<Connection>` 或轻量池）。
axum 多线程 handler 通过共享 `AuthState` 串行化写，避免竞态。

### 4.2 路由划分

| 分组 | 路由 | 是否需登录 | 是否需授权有效 |
|---|---|---|---|
| 公开页 | `/login`、`/register`（或合并页） | 否 | 否 |
| 认证 API | `/api/auth/register`、`/login` | 否 | 否 |
| 认证 API | `/api/auth/{logout,activate,me}` | 是 | 否（activate/me 在锁定态也要能用） |
| 主页面 | `/` | 是 | 否（页面能开，功能靠 API 拦） |
| 核心 API | `/api/run`、`/api/recommend`、`/api/stock/*`、`/api/push/*`、`/api/compare`、`/api/optimize`、`/api/sync`、`/api/funds`、`/api/regime`、`/api/holdings` | 是 | **是** |
| 管理页 | `/admin` | 是 + admin | — |
| 管理 API | `/api/admin/*` | 是 + admin | — |

`auth_gate` 中间件：读 Cookie → 查 session → 查 user；核心 API 要求 user 存在、未封禁且
`LicenseStatus ∈ {Active,Warning,Grace}`，否则返回 `401`（未登录）或 `403`（未授权/过期），
响应体为结构化 JSON `{"error":"unauthorized|license_required|expired","status":"..."}`，
前端据此弹激活提示。`admin_gate`：在 auth_gate 之上再要求 `is_admin`，否则 `404`（不暴露后台存在）。

### 4.3 认证 API 契约

- `POST /api/auth/register` `{username, password}` → 创建用户（`expires_at=NULL`），
  校验用户名唯一、密码长度下限；`open_registration=false` 时拒绝。成功后可选直接建会话登录。
- `POST /api/auth/login` `{username, password}` → argon2 校验 → 建 session、下发 Cookie。封禁用户拒绝。
- `POST /api/auth/logout` → 删除当前 session、清 Cookie。
- `POST /api/auth/activate` `{code}` → 事务内校验码「存在且未用未作废」→ 条件更新占用 → 续期 `expires_at`。返回新到期日与状态。并发/重复用同一码返回明确错误。
- `GET /api/auth/me` → `{username, is_admin, expires_at, status, warn_days, grace_days, remaining_days}`，供前端顶栏渲染。

### 4.4 管理 API 契约（均需 admin）

- `POST /api/admin/codes` `{days, count}` → 批量生成，返回码列表。
- `GET  /api/admin/codes?filter=unused|used|all` → 列表（码/天数/使用者/时间/是否作废）。
- `POST /api/admin/codes/revoke` `{code}` → 作废未用码。
- `GET  /api/admin/users` → 用户列表（用户名/状态/到期日/是否封禁/是否管理员）。
- `POST /api/admin/users/extend` `{user_id, days}` → 手动续期。
- `POST /api/admin/users/disable` `{user_id, disabled}` → 封禁/解封。
- `POST /api/admin/users/set_admin` `{user_id, is_admin}` → 授/撤管理员。
- `GET  /api/admin/overview` → 概览统计（用户数、在用数、临期数、最近激活记录）。

## 5. CLI 子命令（`main.rs`）

网页后台需要一个管理员才能进（鸡生蛋），故**首个管理员必须用 CLI 引导**：

- `xlh admin create --username <name>` → 交互式设密码，创建 `is_admin=1` 用户（若无管理员则允许）。
- `xlh license issue --days <n> --count <c>` → 打印新授权码（与网页发码等价，二者皆可）。
- `xlh license list [--filter unused|used|all]` → 列码。
- `xlh user list` → 列用户与状态。

所有 CLI 子命令与 Web 共用同一 `store.rs`，操作同一 `db_path`。

## 6. 配置 `config.toml` 新增 `[auth]`

```toml
[auth]
db_path = "data/xlh.db"       # SQLite 文件路径
open_registration = true       # 是否开放自助注册
warn_days = 7                  # 到期前多少天开始提醒
grace_days = 3                # 到期后宽限多少天仍可用
session_ttl_days = 30         # 会话有效期
# session_secret 若缺省，首次启动自动生成随机值并写回文件；无需手填
```

缺省值内建，`[auth]` 整段缺失时用默认值，保证老配置可平滑升级。

## 7. 前端

- 未登录访问 `/` → 重定向 `/login`。登录/注册/激活合并为一个 `page_auth` 页（含激活码输入框）。
- 已登录主页面 `page.rs` 顶部注入**授权状态栏**：调用 `/api/auth/me`，按 `status` 渲染
  绿/黄/红三色 + 到期日/剩余天数 + 「激活/续期」输入框 + 退出登录；管理员多一个「管理后台」入口。
- 核心 API 收到 `403 license_required/expired` 时，前端统一弹激活对话框，不让功能静默失败。
- 管理后台 `/admin` 为独立页：发码板块、用户板块、概览板块（表格 + 操作按钮，调 `/api/admin/*`）。

## 8. 安全要点

- 密码：argon2（PHC 串含盐），绝不明文/裸 SHA。
- 会话：token 由 `rand` CSPRNG 生成，Cookie 设 `HttpOnly`、`SameSite=Lax`；生产（HTTPS）下 `Secure`。
- 授权码：足够长的随机串，一次性；DB 条件更新保证并发不重复占用。
- 管理后台：`admin_gate` 双重校验；非管理员访问返回 404 不暴露存在。
- 时序：登录失败不区分「用户不存在/密码错」文案，减少枚举。

## 9. 测试策略

- 纯函数 `LicenseStatus::of` 全分支单测（未激活/正常/临期/宽限/过期边界）。
- `store.rs` 用内存 SQLite 单测：注册唯一性、激活续期语义、一次性码并发占用（模拟两次激活只成功一次）。
- 激活续期数学：未过期续期（从原到期叠加）、过期续期（从今天起算）两条路径。
- 中间件：无 Cookie→401；已登录未激活→403；封禁→拒绝；admin_gate 非管理员→404。
- 沿用现有测试风格；联网/端到端冒烟保持 `#[ignore]`。

## 10. 非目标（YAGNI，本期不做）

- 在线支付/自动发码、发票、分销。
- 功能档位/分版本（授权码只带时长）。
- 私有部署 / 离线 License / 绑机器。
- 多租户数据隔离、找回密码邮件、2FA（可作为后续独立子项目）。

## 11. 交付影响

- 依赖新增：`rusqlite`（bundled feature）、`argon2`、`rand`。
- 部署：`data/` 目录需持久化（docker-compose 挂卷，类似现有 `.cache`/`output`）；
  首次部署后跑一次 `xlh admin create` 引导管理员。
- 对现有基金/股票分析代码零改动，仅在 `web::router()` 外层加认证/管理路由与中间件。
