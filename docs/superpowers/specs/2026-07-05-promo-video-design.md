# xlh 宣传视频（Remotion 动画）—— 设计文档

- 日期：2026-07-05
- 状态：已确认，待实现
- 关联：[[saas-license-plan]]、[[stock-system-plan]]

## 1. 目标

为 xlh 投资研判系统制作**动画宣传视频**，投放抖音/快手/小红书（竖屏 9:16）与哔哩哔哩（横屏 16:9）。
内容需覆盖：系统功能、为用户解决的问题、核心优势，并以微信号 CTA 收尾。

交付方式：**Remotion（React 程序化视频）工程**，用户本地 `npm i` + `npx remotion render` 渲染出真正的 MP4。
另附**文案包**（配音口播稿 + 各平台标题/正文/话题标签）。

## 2. 核心决策（已确认）

| 维度 | 决策 |
|---|---|
| 交付形态 | Remotion 工程，渲染 MP4（非成片直出，用户本地渲染） |
| 画幅 | 双版本：竖屏 9:16（1080×1920，抖音/快手/小红书）+ 横屏 16:9（1920×1080，B站） |
| 时长 | ~60s，30fps，1800 帧，8 场景 |
| 音频 | 画面+字幕驱动；预留旁白/BGM 音轨槽，默认关闭（缺文件会致渲染失败），用户放入 `public/` 再启用 |
| 视觉 | 暗色科技感；A股惯例红涨绿跌（涨/买 `#c0392b`，跌/卖 `#27ae60`），主强调蓝 `#3b82f6` |
| 中文字体 | `@remotion/google-fonts/NotoSansSC`（免手动装字体） |
| CTA | 微信号 **I1346535693** |
| 位置 | 独立 Node 工程置于仓库根 `promo/`，不触碰 Rust 代码；`promo/node_modules` 与 `promo/out` 进 `.gitignore` |

## 3. 分镜脚本（8 场景 × ~7.5s，1800 帧@30fps）

每场景含：帧区间、画面动效、字幕（屏显大字）、配音口播（VO）。字幕为必需，VO 供 TTS 使用。

| # | 帧 | 场景 | 画面动效 | 字幕 | 配音 VO |
|---|---|---|---|---|---|
| 1 | 0–210 | 痛点钩子 | 暗底，多根 K 线剧烈闪烁、红绿乱跳，问号浮现 | 「追涨杀跌？该加仓还是减仓？」 | 追涨杀跌，该加还是该减？凭感觉，其实就是在赌。 |
| 2 | 210–420 | 系统亮相 | Logo「xlh」弹入(spring)+光晕扩散，副标题淡入 | 「xlh 投资研判系统 · 让每个决策有数据撑腰」 | 认识一下 xlh 投资研判系统——让每一个买卖决策，都有数据撑腰。 |
| 3 | 420–720 | 回测+寻优 | 收益曲线沿时间轴生长；右侧参数网格逐格点亮、锁定最优格 | 「定投/择时回测 · 参数寻优」 | 定投还是择时？哪套参数最优？先用历史数据跑回测、自动寻优，用结果说话。 |
| 4 | 720–990 | 诊断+多市场 | 三色状态灯切换(上涨/下跌/震荡)；A股/港股/美股三标签依次浮现 | 「市场状态诊断 · A股/港股/美股全覆盖」 | 当下是涨、是跌、还是震荡？一眼诊断。A股、港股、美股，全都覆盖。 |
| 5 | 990–1290 | 选股+持仓建议 | 候选股卡片按评分滑入排名；持仓逐行贴 加仓/持有/减仓/止盈 标签+建议金额 | 「跨股选股 · 逐只持仓建议（加/持/减/止盈+金额）」 | 跨股票帮你选优，再对你的每一只持仓，给出加仓、持有、减仓还是止盈，连建议金额都算好。 |
| 6 | 1290–1530 | 自动推送 | 手机外框中弹出微信/钉钉/飞书三张消息卡片依次滑入 | 「定时推送到微信/钉钉/飞书——不盯盘」 | 到点自动同步数据、生成建议，直接推送到你的微信、钉钉、飞书。不用天天盯盘。 |
| 7 | 1530–1710 | 可回溯+可控 | 历史建议时间线横向滑动；盾牌图标 + 「本地/私有部署」 | 「持仓建议历史可回溯 · 私有部署数据自控」 | 每次建议都存档，决策可回溯；本地私有部署，数据牢牢在你自己手里。 |
| 8 | 1710–1800 | CTA | 4 个优势 icon 收束(数据驱动/多市场/自动省心/轻量私有)；微信号大字+二维码占位 | 「注册授权即用 · 微信 I1346535693」 | 数据驱动，多市场，自动省心。现在就上手——微信 I1346535693。 |

> 场景边界用整帧对齐；实现允许 ±30 帧微调以适配动画节奏。

## 4. Remotion 工程架构

```
promo/
  package.json            # remotion ^4, react 18, @remotion/google-fonts, typescript
  tsconfig.json
  remotion.config.ts      # 视频质量/编码设置
  .gitignore              # node_modules / out
  public/                 # (用户放) narration.mp3 / bgm.mp3
  src/
    index.ts              # registerRoot(Root)
    Root.tsx              # 定义 PromoVertical(1080x1920) 与 PromoWide(1920x1080)，均 durationInFrames=1800 fps=30
    theme.ts              # COLORS/字体加载/FPS/TOTAL/场景帧表 SCENES
    Promo.tsx             # 主装配：<Series> 依 SCENES 串 8 场景 + <AudioTrack/>
    components/
      Bg.tsx              # 暗色渐变+网格背景(所有场景共用底)
      Caption.tsx         # 屏显大字，spring 淡入+上移；竖/横字号自适应
      Candles.tsx         # K线柱随机涨跌动画(场景1)
      GrowLine.tsx        # 收益曲线沿帧生长(场景3)
      ParamGrid.tsx       # 参数网格点亮+锁定最优(场景3)
      StatusLight.tsx     # 三色市场状态灯(场景4)
      MarketTags.tsx      # A股/港股/美股标签(场景4)
      StockCard.tsx       # 候选股排名卡(场景5)
      HoldingRow.tsx      # 持仓行+动作标签+金额(场景5)
      PhoneNotify.tsx     # 手机+消息卡片(场景6)
      Timeline.tsx        # 历史时间线(场景7)
      Bullets.tsx         # 优势 icon 列(场景8)
      QRPlaceholder.tsx   # 二维码占位方块+微信号(场景8)
      AudioTrack.tsx      # 可选旁白+BGM（默认返回 null，注释说明如何启用）
    scenes/
      S1Hook.tsx ... S8Cta.tsx   # 每场景组装自身组件，用 useVideoConfig() 判定 orientation 重排
```

**自适应**：场景组件调 `useVideoConfig()` 得 `width/height`；`const vertical = height >= width`。竖屏内容纵向堆叠、字号偏大；横屏并排、字号偏中。用 flex + 百分比，不写死像素。

**时间常量（theme.ts）**：`FPS=30`、`TOTAL=1800`、`SCENES=[{name,from,durationInFrames}...]`（8 项，帧值同 §3）。`Promo.tsx` 用 `<Series>` 按 SCENES 渲染，改时长只改此表。

**音轨**：`AudioTrack` 默认 `return null`（避免缺文件渲染报错）；文件注释块给出启用代码：
`<Audio src={staticFile('narration.mp3')} />` + `<Audio src={staticFile('bgm.mp3')} volume={0.15} />`。

**字体**：`theme.ts` 顶部 `loadFont()`（@remotion/google-fonts/NotoSansSC），导出 `fontFamily`。

## 5. 文案包（`promo/COPY.md`）

- **配音口播稿**：§3 的 8 段 VO 汇总（可直接喂 TTS/剪映朗读）。
- **各平台适配**：
  - 抖音/快手：强钩子（前 3 秒文案）、建议卡点、15/30/60s 剪切建议。
  - 小红书：标题（≤20字）+ 正文 + 话题标签。
  - B站：标题 + 简介 + 分区（科技/财经）+ 标签。
- **话题标签池**：#基金 #股票 #A股 #港股 #美股 #量化投资 #理财工具 #定投 #择时 #回测 #投资研判 等，按平台挑选。

## 6. 渲染与使用（写入 `promo/README.md`）

```bash
cd promo
npm install
npx remotion studio                                   # 预览/调参
npx remotion render PromoVertical out/xlh-9x16.mp4     # 竖屏
npx remotion render PromoWide     out/xlh-16x9.mp4     # 横屏
```
- 加音频：把 `narration.mp3`/`bgm.mp3` 放 `public/`，按 `AudioTrack.tsx` 注释启用后再渲染。
- 改时长/节奏：编辑 `theme.ts` 的 `SCENES` 帧表与 `TOTAL`。
- 替换二维码：把真实二维码图放 `public/qr.png`，在 `QRPlaceholder.tsx` 换成 `<Img>`。

## 7. 非目标（YAGNI）

- 不做配音音频生成（我产不了音轨；预留槽由用户填）。
- 不做真实二维码生成（占位；用户替换）。
- 不接入 xlh 真实运行截图/实时数据（用风格化数据可视化表意，避免耦合与联网）。
- 不做多语言/英文版（本期中文）。
- 不改动 Rust 主程序任何代码。

## 8. 验收标准

- `promo/` 工程 `npm install` 后，`npx remotion render` 能成功产出 `PromoVertical` 与 `PromoWide` 两个 MP4（默认无音频，不报错）。
- 两版均 1800 帧、包含全部 8 场景，字幕与动效按 §3 呈现，竖/横布局各自合理不溢出。
- 红涨绿跌配色、暗色风格、NotoSansSC 中文正常显示（无方块/豆腐块）。
- `COPY.md` 含 8 段 VO + 四平台文案 + 话题标签；`README.md` 含渲染步骤。
- CTA 显示微信号 I1346535693。
