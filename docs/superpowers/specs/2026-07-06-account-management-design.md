# 用户账户管理设计（改密 + 注销/删除 + 注册上限）

日期：2026-07-06
状态：已批准，待实现

## 背景

xlh 已从工具转为收费 SaaS：账号 + 授权码激活，SQLite 存储，会话 cookie
鉴权，管理后台位于 `/admin`（见 `src/web/auth/`）。当前用户账户栏
（`src/web/page.rs` 的 `#xlh-bar`）只能查看授权状态、激活/续期、退出；管理后台
可发码、续期、封禁、设管理。本设计在此基础上补齐三块账户管理能力：

1. 用户自助修改密码 + 管理员重置用户密码
2. 账号注销与删除（仅管理员后台，两级规则保护付费数据）
3. 未激活账号注册上限（反滥用）

## 目标与非目标

**目标**
- 登录用户可在账户栏自助改密；改密后除当前会话外，其余设备会话失效。
- 管理员可在后台重置任意用户密码（无需旧密码），目标用户全设备强制重登。
- 管理员可注销 / 删除账号：未激活账号可直接删；已激活账号须先注销再删。
- 未激活用户数达 10000 时拒绝新注册。

**非目标**
- 用户自助删除/注销自己的账号（本期仅管理员执行）。
- 找回密码 / 邮箱验证 / 密码复杂度策略（沿用现有「≥6 位」）。
- 授权码历史（`codes.used_by`）在删号时的清理——保留作历史记录。

## 数据模型

`users` 表新增一列：

```
cancelled_at TEXT   -- NULL = 正常；非 NULL = 已注销（记录注销日期）
```

**迁移**：`store::migrate()` 现仅执行 `CREATE TABLE IF NOT EXISTS`。为兼容旧库，
迁移中对 `users` 做幂等加列——先 `PRAGMA table_info(users)` 检查是否已有
`cancelled_at`，缺列才 `ALTER TABLE users ADD COLUMN cancelled_at TEXT`。
新库由 `CREATE TABLE` 直接包含该列。两条路径都要覆盖。

`User` 模型（`model.rs`）新增派生字段：

```rust
pub cancelled: bool,   // = (cancelled_at IS NOT NULL)
```

读查询 `find_user_by_name` / `find_user_by_id` / `list_users` 三处 SELECT 补上
`cancelled_at` 列并映射为 `cancelled`。

## Store 层新增函数（`store.rs`）

| 函数 | 语义 |
|---|---|
| `pw_hash_by_id(conn, user_id) -> Result<Option<String>>` | 按 id 取 `pw_hash`，供自助改密校验旧密码 |
| `update_password(conn, user_id, new_hash) -> Result<()>` | `UPDATE users SET pw_hash` |
| `delete_sessions_except(conn, user_id, keep: Option<&str>) -> Result<usize>` | 删除该用户 session；`Some(token)` 保留当前会话，`None` 全删；返回删除行数 |
| `set_cancelled(conn, user_id, cancelled: bool) -> Result<()>` | `cancelled=true` 写入当日 `cancelled_at`，`false` 置 NULL（恢复） |
| `delete_user(conn, user_id) -> Result<()>` | 事务内：删该用户的 sessions，再删 users 行。`codes.used_by` 保留为历史，不清理 |
| `count_unactivated(conn) -> Result<i64>` | `SELECT COUNT(*) FROM users WHERE expires_at IS NULL AND cancelled_at IS NULL` |

## 访问控制：已注销 = 不可登录、不可用

「已注销」与「封禁」语义独立，但对访问的效果相同：均拦截登录与会话使用。

- `CurrentUser`（`mod.rs`）新增 `cancelled: bool`，由 `User` 映射。
- `require_login`：现放行条件 `Some(u) if !u.disabled`，改为
  `!u.disabled && !u.cancelled`，否则 401。
- `login`（`handlers.rs`）：命中用户后的判定由 `user.disabled || !ok` 扩展为
  `user.disabled || user.cancelled || !ok` → 401 `invalid_login`（不泄露账号是否
  被注销，沿用统一错误）。

## 改密

### 自助（`handlers.rs`，挂 authed 组，不要求 license）

请求：`POST /api/auth/change_password`

```json
{ "current_password": "...", "new_password": "..." }
```

处理 `change_password(State, Extension<CurrentUser>, headers, Json)`：
1. `new_password` 字符数 < 6 → 400 `invalid_password`。
2. `pw_hash_by_id(user.id)` 取当前 hash（以 id 为准，避免用户名竞态）；取后立即释放锁。
3. `password::verify(current_password, hash)` 失败 → 400 `wrong_password`。
4. `password::hash(new_password)` 失败 → 500 `hash_failed`。
5. `update_password`，失败 → 500 `update_failed`。
6. `delete_sessions_except(user.id, Some(当前 cookie token))` —— 注销其他设备，
   保留当前会话（token 由 `session::read_cookie(headers)` 取得）。
7. 返回 `{ "ok": true }`。

慢速 argon2 校验期间**不得持有 DB 锁**（沿用 `login` 的模式：先取 hash 释放锁，
锁外校验，再短暂持锁写库）。

路由：`authed` 组新增 `.route("/api/auth/change_password", post(change_password))`。

### 管理员重置（`admin.rs`）

请求：`POST /api/admin/users/reset_password { user_id, new_password }`

处理：
1. `new_password` < 6 → 400 `invalid_password`。
2. `find_user_by_id` 不存在 → 404 `user_not_found`。
3. hash → `update_password` → `delete_sessions_except(user_id, None)`（全设备重登）。
4. 返回 `{ "ok": true }`。无需旧密码。

## 注销 / 删除（仅管理员后台）

### 注销 / 恢复

`POST /api/admin/users/cancel { user_id, cancelled: bool }`
- `cancelled=true`：**末位管理员保护**——若目标是启用中的唯一管理员则 400 `last_admin`；
  否则 `set_cancelled(id, true)` 并 `delete_sessions_except(id, None)`（立即踢下线）。
- `cancelled=false`：`set_cancelled(id, false)` 恢复。

### 删除

`POST /api/admin/users/delete { user_id }`
1. `find_user_by_id` 不存在 → 404 `user_not_found`。
2. **末位管理员保护**：启用中的唯一管理员 → 400 `last_admin`。
3. 删除规则：`expires_at IS NULL`（未激活）**或** `cancelled`（已注销）→ 允许；
   否则（已激活且未注销）→ 400 `must_cancel_first`。
4. `delete_user(id)` → `{ "ok": true }`。

「末位管理员」判定复用 `count_admins`（统计启用中的管理员），与现有
`disable_user` / `set_admin` 保护一致。

## 注册上限（`handlers.rs::register`）

在现有用户名 / 密码合法性校验之后、`create_user` 之前插入：

```
if count_unactivated(&conn) >= 10000 { return 403 registration_full }
```

## 前端

### 账户栏改密（`page.rs`）
- `#xlh-bar` 新增「修改密码」按钮（退出按钮旁）。
- 点击弹出内嵌深色模态（overlay + 卡片），三个密码框：旧密码 / 新密码 / 确认新密码，
  提交 / 取消。
- JS 前置校验：新密码 = 确认、长度 ≥ 6；`POST /api/auth/change_password`；
  成功 `alert` 并关闭（当前会话仍有效），失败按错误码映射中文
  （`wrong_password`→旧密码错误，`invalid_password`→新密码至少 6 位）。

### 管理后台（`admin.rs` 的 `ADMIN_HTML`）
- 用户表「操作」列在现有按钮后增加：
  - 「重置密码」→ `prompt()` 输入新密码 → `POST /reset_password`（运营者工具，prompt 足够）。
  - 「注销 / 恢复」→ `POST /cancel`（按当前 `cancelled` 切换）。
  - 「删除」→ `confirm()` 二次确认 → `POST /delete`；`must_cancel_first` 提示「请先注销该已激活账号」。
- `loadUsers` 渲染需能反映注销态（如用户名后标注「(已注销)」）。

## 错误码汇总

| 码 | HTTP | 场景 |
|---|---|---|
| `invalid_password` | 400 | 新密码 < 6 位 |
| `wrong_password` | 400 | 自助改密旧密码不符 |
| `registration_full` | 403 | 未激活用户数达 10000 |
| `must_cancel_first` | 400 | 删除已激活且未注销账号 |
| `last_admin` | 400 | 注销/删除唯一启用管理员 |
| `user_not_found` | 404 | 管理端目标用户不存在 |
| `hash_failed` | 500 | argon2 hash 失败 |
| `update_failed` | 500 | DB 写失败 |

## 测试（TDD）

**store（`store.rs` 单测）**
- `update_password` 改变 hash，旧 verify 失效、新 verify 通过。
- `delete_sessions_except`：`Some(keep)` 保留该 token 删其余；`None` 全删；返回计数正确。
- `set_cancelled` 写/清 `cancelled_at`，`find_user_by_id` 的 `cancelled` 随之变化。
- `delete_user` 删除用户及其 sessions（`lookup_session_user` 返回 None）。
- `count_unactivated` 排除已激活与已注销用户。
- 迁移：对不含 `cancelled_at` 的旧库 schema 执行 `migrate()` 后该列存在且可读。

**handlers（路由级）**
- 改密：旧密码错 → 400；新密码过短 → 400；成功 → 200，其他会话失效、当前会话仍可用。
- 注册：未激活数达上限 → 403 `registration_full`。
- login：已注销用户 → 401。

**admin（路由级，复用 `seed_user`/`post_admin`）**
- `reset_password`：成功 200，目标会话清空。
- `cancel`：注销后目标无法登录、会话被清。
- `delete`：删未激活 → 200；删「已激活未注销」→ 400 `must_cancel_first`；
  删「已激活已注销」→ 200。
- 末位管理员：注销/删除唯一管理员 → 400 `last_admin`。
- 非管理员访问上述管理端点 → 404（中间件保证，补一条断言）。

## 影响文件

- `src/web/auth/store.rs`（schema + 迁移 + 新函数 + 单测）
- `src/web/auth/model.rs`（`User.cancelled`）
- `src/web/auth/mod.rs`（`CurrentUser.cancelled` + `require_login`）
- `src/web/auth/handlers.rs`（`change_password` + register 上限 + login 注销拦截）
- `src/web/auth/admin.rs`（reset_password / cancel / delete + 后台 HTML）
- `src/web/auth/routes.rs`（新增 3 条管理路由 + 测试）
- `src/web/mod.rs`（authed 组新增 change_password 路由）
- `src/web/page.rs`（改密模态 UI）
