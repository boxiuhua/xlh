# xlh 持仓建议历史 —— 设计文档

- 日期：2026-07-05
- 状态：已确认，待实现
- 关联：[[saas-license-plan]]（授权体系，本功能挂在其账号/授权门之内）

## 1. 背景与目标

「持仓建议」目前有两条生成路径，都用完即弃、不留历史：

- **Web**：`POST /api/holdings`（登录用户在「持仓建议」Tab 录入持仓 → 返回逐只建议 `HoldingsReport`）。
- **定时推送**：`xlh push` job 按 `push.toml` 生成基金持仓建议 → 发到钉钉/飞书等渠道。

目标：把生成的建议**保存为历史**，供事后回看与横向对比（如「上周减仓、本周持有」）。

## 2. 核心决策（已与用户确认）

| 维度 | 决策 |
|---|---|
| 保存来源 | Web 手动保存 + 定时推送自动保存，两者都存 |
| Web 保存触发 | 用户点「保存到历史」按钮（非每次生成自动存） |
| Web 保存权威性 | 后端用提交的 `HoldingsInput` **重跑** `holdings_blocking`，存权威结果（不信任前端已生成的报告） |
| Web 历史隔离 | 按登录用户隔离，只有本人可见/可取 |
| 推送历史可见性 | **仅管理员**（来自操作者自己的 `push.toml`，不泄露给客户） |
| 查看方式 | Web：「持仓建议」面板内「历史」子区；推送：管理后台「推送历史」板块 |
| 存储 | 复用现有 SQLite（`data/xlh.db`），新增 `advice_history` 表 |
| 保留策略 | Web 每用户最新 100 条（超出删旧）；推送历史暂不设上限 |
| 时间戳 | 完整 `YYYY-MM-DD HH:MM:SS`（区分一天多次保存） |

## 3. 架构

新增 **crate 级模块 `src/history.rs`**（asset-neutral，纯 rusqlite，与 auth 解耦，不 use 基金/股票业务模块）。
复用同一 SQLite 库文件 `data/xlh.db`：Web 走现有 `AuthState` 的 `Arc<Mutex<Connection>>`；
`xlh push` 是独立进程，自行开一个连接连同一库（WAL 支持跨进程；push 写入低频，竞争可忽略）。

```
src/history.rs           历史存储：AdviceRecord/migrate/save/list_web/get_web/list_push/get_push
src/web/mod.rs           Web serve 里 history::migrate；新增 3 个 /api/holdings/{save,history,history/:id} 处理器 + 路由（授权组）
src/web/auth/admin.rs    新增 push-history 列表/详情处理器
src/web/auth/routes.rs   admin_router 挂 /api/admin/push-history[/:id]
src/web/page.rs          「持仓建议」面板加「保存到历史」按钮 + 「历史」子区；管理后台页加「推送历史」板块
src/push/job.rs          生成基金持仓建议后 history::save(source=push, user_id=None)
src/main.rs              push 分支/守护打开历史库（经 config::load_auth 取 db_path）
```

## 4. 数据模型

### 4.1 表 `advice_history`

```sql
CREATE TABLE IF NOT EXISTS advice_history (
  id         INTEGER PRIMARY KEY,
  user_id    INTEGER,            -- web: 保存者 user id；push: NULL（操作者全局）
  source     TEXT NOT NULL,      -- 'web' | 'push'
  created_at TEXT NOT NULL,      -- ISO datetime 'YYYY-MM-DD HH:MM:SS'
  summary    TEXT NOT NULL,      -- 列表摘要
  payload    TEXT NOT NULL       -- JSON：{ "input": HoldingsInput, "report": HoldingsReport }
);
CREATE INDEX IF NOT EXISTS idx_advice_web  ON advice_history(user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_advice_push ON advice_history(source, created_at);
```

### 4.2 结构体（`src/history.rs`）

```rust
pub struct AdviceRecord {         // 列表项（不含 payload）
    pub id: i64,
    pub created_at: String,
    pub summary: String,
}
```

- `payload` 存储 `{ "input": <HoldingsInput>, "report": <HoldingsReport> }` 的 JSON 字符串，
  详情接口原样返回该 JSON（前端据此渲染录入项 + 逐只建议）。
- `summary` 由后端从 `HoldingsReport` 计算：形如 `N 只 · 加仓A/持有B/减仓C`（动作分布计数）。

### 4.3 存储接口（`src/history.rs`，纯函数，`&Connection`）

- `migrate(conn) -> Result<()>`：建表+索引（`IF NOT EXISTS`，幂等）。
- `save(conn, user_id: Option<i64>, source: &str, summary: &str, payload: &str) -> Result<i64>`：
  插入一条，返回 id；**若 `source=="web"` 且 `user_id=Some`**，插入后删除该用户超出最新 100 条的旧记录。
- `list_web(conn, user_id: i64, limit: i64) -> Result<Vec<AdviceRecord>>`：该用户 web 记录，`created_at DESC`。
- `get_web(conn, id: i64, user_id: i64) -> Result<Option<String>>`：返回属于该用户的记录 payload；不属于则 None。
- `list_push(conn, limit: i64) -> Result<Vec<AdviceRecord>>`：`source='push'` 记录，倒序。
- `get_push(conn, id: i64) -> Result<Option<String>>`：push 记录 payload。

保留裁剪 SQL（删旧，仅 web + 指定用户）：
```sql
DELETE FROM advice_history
 WHERE source='web' AND user_id=?1
   AND id NOT IN (
     SELECT id FROM advice_history
      WHERE source='web' AND user_id=?1
      ORDER BY created_at DESC, id DESC LIMIT 100);
```

## 5. Web 接口（授权组：需登录 + 授权有效；按用户隔离）

- `POST /api/holdings/save`：body = `HoldingsInput`。后端调用现有 `holdings_blocking(input)` 重跑得到权威 `HoldingsReport`，
  计算 `summary`，`payload = {input, report}` JSON，`history::save(Some(user_id), "web", summary, payload)`，返回 `{ "ok": true, "id": <id> }`。
  `holdings_blocking` 为 CPU/IO 型，放 `spawn_blocking`（与现有 `/api/holdings` 一致）。
- `GET /api/holdings/history`：返回当前用户 web 记录列表 `[{id, created_at, summary}]`（limit 100）。
- `GET /api/holdings/history/:id`：返回 `get_web(id, current_user)` 的 payload JSON；None → 404。

三者读取 `CurrentUser`（`Extension<CurrentUser>`），user_id 取自会话，前端无法伪造他人 id。

## 6. 管理接口（管理员组：需登录 + is_admin）

- `GET /api/admin/push-history`：`list_push`，返回 `[{id, created_at, summary}]`。
- `GET /api/admin/push-history/:id`：`get_push` 的 payload；None → 404。

## 7. 推送端保存

`src/push/job.rs` 生成基金持仓建议 `report`（`HoldingsReport`）后，
计算 `summary`、`payload={input, report}`，`history::save(conn, None, "push", summary, payload)`。
连接来源：`xlh push` 启动时按 `config::load_auth(&config_path).db_path`（默认 `data/xlh.db`）`history::open_or_default` 开库并 `migrate`。
保存失败仅告警、不阻断推送（历史是附带能力，不能影响主推送）。
> 说明：push job 现按 `push.toml` 运行，db_path 来自 `config.toml` 的 `[auth]` 段（缺失则默认 `data/xlh.db`）。

## 8. 前端

- **持仓建议面板**：生成建议成功后，展示「保存到历史」按钮 → POST `/api/holdings/save`（body 复用当前录入的 `HoldingsInput`）→ 提示已保存。
  面板内新增「历史」子区：进入时 GET `/api/holdings/history` 渲染列表（时间 + 摘要），点某条 GET 详情并展开渲染录入项 + 逐只建议。
- **管理后台 `/admin`**：新增「推送历史」板块，GET `/api/admin/push-history` 列表 + 点开详情。

## 9. 测试策略

- `history.rs` 内存 SQLite 单测：
  - save + list_web + get_web 往返；
  - **用户隔离**：用户 A 的 `get_web(id, B)` 返回 None；`list_web(B)` 不含 A 的记录；
  - web 与 push 分离：`list_web` 不含 push 记录，`list_push` 不含 web 记录；
  - 保留上限：某用户存 >100 条后，`list_web` 恰 100，最旧被删，push 记录不受影响。
- Web 集成测试（oneshot）：未登录 `POST /api/holdings/save` → 401；已登录未激活 → 403；已登录已激活 → 200 且能在 `/api/holdings/history` 查到。
- 沿用现有测试风格；联网/端到端保持 `#[ignore]`。

## 10. 非目标（YAGNI，本期不做）

- 单条历史删除、历史对比视图、导出。
- 推送历史保留上限（低频，暂不限）。
- 股票持仓建议历史（本期仅基金持仓建议；结构可复用，后续扩展）。
- 历史内容加密。

## 11. 交付影响

- 新增文件 `src/history.rs`；改动 `web/mod.rs`、`web/auth/{admin.rs,routes.rs}`、`web/page.rs`、`push/job.rs`、`main.rs`。
- 复用 `data/xlh.db`，已在持久化卷内；新增表随 `migrate` 幂等创建，老库平滑升级。
- 对持仓建议核心算法（`src/holdings.rs`）零改动，仅在其外围加保存/查询。
