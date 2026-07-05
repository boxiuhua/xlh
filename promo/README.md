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

## 音频集成

### 添加配音与背景音乐

1. **准备音频文件**  
   将配音与 BGM 文件放入 `public/` 目录：
   - `public/narration.mp3` — 配音
   - `public/bgm.mp3` — 背景音乐

2. **启用音轨**  
   编辑 `src/components/AudioTrack.tsx`，取消注释相关代码以启用：
   ```tsx
   // 例如：
   // <Audio src={staticFile('narration.mp3')} />
   // <Audio src={staticFile('bgm.mp3')} />
   ```

3. **重新渲染**  
   ```bash
   npm run render:v  # 或 npm run render:w
   ```

## 二维码配置

### 替换二维码图片

1. **准备二维码**  
   将二维码 PNG 图片放入 `public/qr.png`

2. **更新组件**  
   编辑 `src/components/QRPlaceholder.tsx`，修改：
   ```tsx
   // 从占位符：
   // <Rect ... />
   
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
  { name: 'S1Hook', durationInFrames: 210 },      // 场景 1：钩子
  { name: 'S2Logo', durationInFrames: 210 },      // 场景 2：亮相
  { name: 'S3Backtest', durationInFrames: 300 },  // 场景 3：回测寻优
  // ... 其他场景
  { name: 'S8Cta', durationInFrames: 90 },        // 场景 8：CTA
];

export const TOTAL = 1800; // 总帧数 = 总秒数 × 30fps（默认30fps）
```

**调整逻辑**
- 每个场景帧数对应时长：帧数 ÷ 30 = 秒数（基于 30fps）
- 总时长调整：修改 `TOTAL` 值（帧数）或改变场景帧数总和（TOTAL=1800 帧 ≈ 60s）
- 修改后重新渲染即生效

## 文案资料

完整的配音口播稿、各平台文案和话题标签库，见 **`COPY.md`**。

---

## 工程结构

```
promo/
├── src/
│   ├── index.ts                    # 入口点
│   ├── Promo.tsx                   # 主 Composition（两版注册）
│   ├── theme.ts                    # 主题、SCENES、COLORS、FPS 常量
│   ├── components/
│   │   ├── Bg.tsx                  # 背景
│   │   ├── Caption.tsx             # 文本字幕组件
│   │   ├── AudioTrack.tsx          # 音轨（占位）
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
│   │   └── QRPlaceholder.tsx       # 二维码占位
│   └── scenes/
│       ├── S1Hook.tsx              # 场景 1：钩子
│       ├── S2Logo.tsx              # 场景 2：亮相
│       ├── S3Backtest.tsx          # 场景 3：回测
│       ├── S4Diagnose.tsx          # 场景 4：诊断
│       ├── S5Picks.tsx             # 场景 5：选股/持仓建议
│       ├── S6Push.tsx              # 场景 6：推送
│       ├── S7History.tsx           # 场景 7：历史存档
│       └── S8Cta.tsx               # 场景 8：CTA
├── public/                         # 静态资源（音频、二维码、图片）
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
