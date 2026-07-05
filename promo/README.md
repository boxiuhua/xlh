# xlh 投资研判系统 — 宣传片 Remotion 工程

本工程使用 [Remotion](https://www.remotion.dev) 生成 xlh 投资研判系统的宣传视频，输出竖屏（9:16）和横屏（16:9）两版 MP4。

## 环境要求

- **Node.js** ≥ 18
- **npm** ≥ 8（或其他包管理器）
- **联网环境**（首次渲染会下载无头 Chrome）

### ⚠️ Windows 沙箱环境警告

在 Windows 沙箱环境下，ffmpeg 拼帧可能因 DLL 初始化失败（错误代码 0xC0000142）而崩溃。  
**解决方案**：请在正常（非沙箱）终端执行渲染命令。

## 快速开始

```bash
cd promo
npm install
npm run studio            # 预览编辑（本地 http）
npm run render:v          # 竖屏 9:16 渲染 → out/xlh-9x16.mp4
npm run render:w          # 横屏 16:9 渲染 → out/xlh-16x9.mp4
```

## 配音（已内置 AI 配音）

宣传片已内置 **8 段 AI 配音**（用 [edge-tts](https://github.com/rany2/edge-tts) 微软神经网络 TTS 生成，音色 `zh-CN-YunxiNeural` 阳光男声），文件在 `public/vo/s1.mp3 … s8.mp3`，由 `src/components/AudioTrack.tsx` 按各场景起始帧对齐播放。渲染出的 MP4 已含配音音轨（aac 立体声）。

### 换音色 / 改配音文案

1. 安装 edge-tts（需 Python）：`pip install edge-tts`
2. 重新生成某一段（示例：把场景 3 换成温柔女声）：
   ```bash
   python -m edge_tts --voice zh-CN-XiaoxiaoNeural --rate "+8%" \
     --text "你的新文案" --write-media public/vo/s3.mp3
   ```
   常用中文音色：`zh-CN-YunxiNeural`(阳光男)、`zh-CN-YunyangNeural`(新闻男)、`zh-CN-YunjianNeural`(激情男)、`zh-CN-XiaoxiaoNeural`(温柔女)、`zh-CN-XiaoyiNeural`(活泼女)。查看全部：`python -m edge_tts --list-voices`
3. 各段口播稿见 `COPY.md`；改完 `npm run render:v` / `render:w` 重渲即生效。
   > 若新配音比对应场景更长，会与下一段叠音——可提高 `--rate` 压缩时长，或在 `src/theme.ts` 调整该场景帧数。

### 加背景音乐（可选）

把 `bgm.mp3` 放入 `public/`，在 `AudioTrack.tsx` 里加一行 `<Audio src={staticFile('bgm.mp3')} volume={0.12} />`，重渲即可。

## 二维码配置

### 替换二维码图片

1. **准备二维码**  
   将二维码 PNG 图片放入 `public/qr.png`

2. **更新组件**  
   编辑 `src/components/QRPlaceholder.tsx`，修改：
   ```tsx
   // 从占位符：
   // <div ... />
   
   // 改为实际图片：
   <Img src={staticFile('qr.png')} width={...} height={...} />
   ```

3. **重新渲染**  
   ```bash
   npm run render:v  # 或 npm run render:w
   ```

## 时长与节奏调整

### 修改场景帧数与总时长

编辑 `src/theme.ts` 中的 `SCENES` 数组和 `TOTAL` 常量：

```typescript
// src/theme.ts

export const FPS = 30;

export const SCENES: SceneDef[] = [
  { name: 'S1Hook', durationInFrames: 300 },      // 1 痛点
  { name: 'S2Logo', durationInFrames: 240 },      // 2 亮相
  { name: 'S3Backtest', durationInFrames: 330 },  // 3 回测（真实界面）
  { name: 'SOptimize', durationInFrames: 240 },   // 4 参数寻优
  // ... 共 13 场景 ...
  { name: 'S8Cta', durationInFrames: 240 },       // 13 CTA
];

export const TOTAL = 3600; // 总帧数 = 总秒数 × 30fps
```

**调整逻辑**
- 每个场景帧数对应时长：帧数 ÷ 30 = 秒数（基于 30fps）
- 总时长调整：修改 `TOTAL` 值（帧数），并让场景帧数总和相等（当前 TOTAL=3600 帧 ≈ 120s，13 场景）
- 修改后重新渲染即生效

## 文案资料

完整的配音口播稿、各平台文案和话题标签库，见 **`COPY.md`**。

---

## 工程结构

```
promo/
├── src/
│   ├── index.ts                    # 入口点
│   ├── Root.tsx                    # 注册 PromoVertical/PromoWide 两个 Composition
│   ├── Promo.tsx                   # 场景组装组件（按 SCENES 顺序拼接各场景）
│   ├── theme.ts                    # 主题、SCENES、COLORS、FPS 常量
│   ├── components/
│   │   ├── Bg.tsx                  # 背景
│   │   ├── Caption.tsx             # 文本字幕组件
│   │   ├── AudioTrack.tsx          # 音轨（13 段场景配音，按帧对齐）
│   │   ├── Screenshot.tsx          # 真实界面截图的浏览器窗口框
│   │   ├── Candles.tsx             # K 线蜡烛图
│   │   ├── GrowLine.tsx            # 增长曲线
│   │   ├── ParamGrid.tsx           # 参数网格
│   │   ├── StatusLight.tsx         # 状态指示灯
│   │   ├── MarketTags.tsx          # 市场标签
│   │   ├── StockCard.tsx           # 股票卡片
│   │   ├── HoldingRow.tsx          # 持仓行
│   │   ├── PhoneNotify.tsx         # 手机推送通知
│   │   ├── Timeline.tsx            # 时间轴
│   │   ├── Bullets.tsx             # 要点列表
│   │   └── QRPlaceholder.tsx       # 真实微信二维码
│   └── scenes/                     # 13 场景（顺序见 theme.ts SCENES）
│       ├── S1Hook / S2Logo         # 痛点 / 亮相
│       ├── S3Backtest              # 回测（真实界面截图）
│       ├── SOptimize               # 参数寻优
│       ├── S4Diagnose              # 市场状态诊断
│       ├── SStockTech              # 股票技术诊断(MA/MACD/RSI)
│       ├── SMarket                 # A股/港股/美股 行情
│       ├── SPicks                  # 智能选股排名
│       ├── S5Picks                 # 持仓建议（真实界面截图）
│       ├── S6Push / S7History      # 自动推送 / 历史
│       ├── SStats                  # 数据总览（数字滚动）
│       └── S8Cta                   # CTA
├── public/                         # 静态资源
│   ├── qr.png                      # 微信二维码（身份区已打码）
│   ├── vo/s1..s13.mp3              # 13 段场景配音（edge-tts）
│   └── shots/*.png                 # 真实系统界面截图（回测/持仓）
├── package.json
├── tsconfig.json
├── remotion.config.ts
└── README.md (this file)
```

## 常见问题

**Q: 渲染很慢或卡住了？**  
A: 首次渲染会下载无头 Chrome（~150MB），需要时间和网络。若卡死超过 5 分钟，可 Ctrl+C 退出重试。

**Q: 音频/二维码没出现？**  
A: 确认已在 `public/` 目录放了对应文件，并在组件代码中启用（取消注释）。

**Q: 想改配色或字体？**  
A: 编辑 `src/theme.ts` 中的 `COLORS` 和 `fontFamily`，所有场景会自动应用。

**Q: 输出 MP4 失败（DLL 错误 0xC0000142）？**  
A: 这是 Windows 沙箱环境问题。请在**正常终端**（非虚拟沙箱）执行渲染。

---

**CTA 微信号**：I1346535693

**视频文案**：见 `COPY.md`
