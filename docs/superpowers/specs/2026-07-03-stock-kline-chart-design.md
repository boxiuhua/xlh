# 股票专业 K线图 + 投资参考（Web）设计

日期：2026-07-03
状态：已批准（待实现计划）

## 目标

在 Web 股票 Tab 诊断某只股票时，页面内展示一张专业多面板 K线图（蜡烛 + 成交量 + MACD + RSI，主图叠加 MA 与布林带），与已有的文字诊断（信号 / 打分 / 理由 / 操作提示）并排，构成「投资参考」。

分析引擎已存在（`stock/indicators.rs` + `stock/diagnose.rs`），本项目**新增的是可视化**与把图接入 Web，不重造分析逻辑。

## 渲染方式

服务端 `plotters` 渲染 PNG，新增 `GET /api/stock/chart?code=..&days=..` 返回 `image/png`，页面用 `<img>` 嵌入。

理由：复用现有 plotters 栈（CLI 回测已用它出 `equity.png`）；零前端依赖，保持页面自包含、无 CDN；离线可用；风险最低。取舍：v1 为静态图，无悬停 / 缩放（明确列入「不做」）。

## 图表布局

单张 PNG，4 个纵向、共享 X 轴（日期）的面板，自上而下：

1. **主图**（约 50% 高）：日K蜡烛，**红涨绿跌**（A股惯例）+ MA20 + MA60 + 布林上 / 中 / 下轨。
2. **成交量**（约 15%）：按当日涨跌着色的量柱。
3. **MACD**（约 17%）：DIF、DEA 线 + 柱状（正负分色）。
4. **RSI**（约 18%）：RSI 线 + 30 / 70 参考水平线。

- 默认展示最近 **250 个交易日**（约 1 年）；`days` 查询参数可调（缺省 250）。
- 蜡烛与叠加均用**原始收盘价 / OHLC**（贴近真实盘面）；文字诊断沿用后复权（现状不变），页面标注一句说明二者口径差异。

## 组件拆分（单一职责）

### `stock/indicators.rs`（扩展）
新增滚动 **series 版**指标（预热段留空 / NaN，对齐输入长度）：
- `sma_series(prices, n) -> Vec<Option<f64>>`
- `bollinger_series(prices, n, k) -> Vec<Option<(mid, upper, lower)>>`
- `macd_series(prices, fast, slow, signal) -> (Vec<f64> dif, Vec<f64> dea, Vec<f64> hist)`（复用已有 `ema_series`）
- `rsi_series(prices, n) -> Vec<Option<f64>>`

均为纯函数 + 单测。末值须与现有 last-value 版（`sma`/`bollinger`/`macd`/`rsi`）一致，作为回归约束。

### `stock/chart.rs`（新）
- 入参：`&[StockBar]` + 图表参数（窗口、指标周期，默认取自 `DiagnoseParams` 同款：MA20/60、Boll(20,2)、MACD(12,26,9)、RSI14）。
- 用 plotters 将 4 面板画到内存 PNG，返回 `Vec<u8>`。
- 不含 web / 文件 IO。数据不足时返回一张带「数据不足」文字的占位 PNG（而非 panic）。

### `web/stock.rs`（扩展）
- 新增 `chart_handler`（`GET /api/stock/chart`）：`validate_stock_code` → `cache::load_or_fetch`（沿用 800 天缓存）→ 裁最近 N 日 → `chart::render` → 返回 `image/png`。

### `web/page.rs`（扩展）
- 诊断视图里，在文字结论上方插入 `<img src="/api/stock/chart?code=CODE&days=250">`，含 `onerror` 占位提示。

## 数据流

诊断某股 → 前端并行请求 `/api/stock/diagnose`（现有，文字结论）与经 `<img>` 触发的 `/api/stock/chart`（新，图）→ 后端复用缓存 800 天、裁取最近 250 日、算各 series、plotters 出 PNG → 浏览器并排展示「图 + 文字投资参考」。

## 错误处理

- 数据不足（少于指标预热所需，沿用 `diagnose` 阈值语义）：`chart::render` 返回占位 PNG 写「数据不足」。
- 代码非法：复用 `validate_stock_code` → HTTP 400。
- 抓取失败：`load_or_fetch` 错误 → HTTP 500；前端 `<img onerror>` 显示占位文案。

## 测试

- **indicators series**：对已知序列断言长度对齐、预热段为空、末值与现有 last-value 版一致。
- **chart::render**：断言返回非空字节、以 PNG magic number（`\x89PNG`）开头、正常与「数据不足」分支均不 panic。
- **web**：`/api/stock/chart?code=bad!!code` → 400；合法路径响应 `Content-Type: image/png`。

## 不做（YAGNI）

- 不引前端图表库 / 交互（悬停、缩放、十字光标）——v1 静态图。
- 不做多周期（周K / 月K）、不做画线工具。
- 不改动推送（Feishu）链路。
