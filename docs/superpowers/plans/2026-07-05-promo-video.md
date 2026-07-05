# xlh 宣传视频 (Remotion) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在仓库根 `promo/` 建一个独立 Remotion 工程，渲染出 9:16 与 16:9 两版、~60s、8 场景的 xlh 投资研判系统动画宣传片，并附文案包。

**Architecture:** React/Remotion 程序化视频。两个 Composition（PromoVertical 1080×1920、PromoWide 1920×1080）共用同一 `Promo` 组件，用 `<Series>` 按 `theme.ts` 的 `SCENES` 帧表串 8 个场景；场景组件用 `useVideoConfig()` 判定竖/横自适应布局。中文用 NotoSansSC，音轨预留默认关闭。不触碰 Rust 代码。

**Tech Stack:** Node ≥18（本机 v24）· Remotion 4 · React 18 · TypeScript · @remotion/google-fonts(NotoSansSC)

## Global Constraints

- 工程目录 `promo/`，独立 Node 工程；`promo/node_modules` 与 `promo/out` 加入根 `.gitignore`。不改动任何 Rust/仓库既有文件（除根 `.gitignore` 追加两行）。
- 两 Composition：`PromoVertical` 1080×1920、`PromoWide` 1920×1080；均 `fps=30`、`durationInFrames=1800`。
- 场景帧表（`SCENES`，合计 1800）：S1Hook 210 · S2Logo 210 · S3Backtest 300 · S4Diagnose 270 · S5Picks 300 · S6Push 240 · S7History 180 · S8Cta 90。
- 配色（`COLORS`）：bg0 `#0b1020`、bg1 `#111827`、up/买/涨 `#c0392b`、down/卖/跌 `#27ae60`、accent `#3b82f6`、text `#e5e7eb`、sub `#94a3b8`、gold `#facc15`。
- 中文字体 NotoSansSC；所有文字用 `fontFamily`，确保无豆腐块。
- 自适应：场景组件 `const {width,height}=useVideoConfig(); const vertical = height>=width;` 竖屏纵向堆叠、字号偏大；横屏并排、字号偏中；用 flex/百分比，不写死分辨率。
- CTA 显示微信号 **I1346535693**；二维码用占位方块（`QRPlaceholder`）。
- 音轨 `AudioTrack` 默认 `return null`（缺文件会致渲染失败）；启用代码以注释形式保留。
- 每个任务验证：`npx tsc --noEmit` 必须通过；`npx remotion still src/index.ts <Comp> out/preview-*.png --frame=<f>` 作视觉抽帧（若本机无法启动无头 Chrome/离线则记录并以 tsc 通过为准）。

---

### Task 1: 工程脚手架 + 主题 + 骨架渲染

**Files:**
- Create: `promo/package.json`、`promo/tsconfig.json`、`promo/remotion.config.ts`、`promo/.gitignore`
- Create: `promo/src/index.ts`、`promo/src/Root.tsx`、`promo/src/theme.ts`、`promo/src/Promo.tsx`
- Create: `promo/src/components/Bg.tsx`、`promo/src/components/Caption.tsx`、`promo/src/components/AudioTrack.tsx`
- Create: `promo/src/scenes/S1Hook.tsx` … `S8Cta.tsx`（8 个占位场景，后续任务替换）
- Modify: `.gitignore`（根，追加 `/promo/node_modules` 与 `/promo/out`）

**Interfaces:**
- Produces：`theme.ts` 导出 `fontFamily, FPS=30, TOTAL=1800, COLORS, SCENES`（`SceneDef={name,durationInFrames}`）；`Bg`、`Caption({text,sub?,color?})`、`AudioTrack`；8 个场景组件 `S1Hook..S8Cta`（占位）；`Promo` 主组件；两 Composition。

- [ ] **Step 1: 根 .gitignore 追加**

在 `.gitignore` 末尾追加两行：

```
/promo/node_modules
/promo/out
```

- [ ] **Step 2: 建工程配置文件**

`promo/package.json`：

```json
{
  "name": "xlh-promo",
  "version": "1.0.0",
  "private": true,
  "scripts": {
    "studio": "remotion studio",
    "render:v": "remotion render src/index.ts PromoVertical out/xlh-9x16.mp4",
    "render:w": "remotion render src/index.ts PromoWide out/xlh-16x9.mp4",
    "tsc": "tsc --noEmit"
  },
  "dependencies": {
    "@remotion/google-fonts": "^4.0.0",
    "react": "^18.3.1",
    "react-dom": "^18.3.1",
    "remotion": "^4.0.0"
  },
  "devDependencies": {
    "@types/react": "^18.3.3",
    "typescript": "^5.5.4"
  }
}
```

`promo/tsconfig.json`：

```json
{
  "compilerOptions": {
    "target": "ES2020",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "jsx": "react-jsx",
    "strict": true,
    "noEmit": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "lib": ["ES2020", "DOM"]
  },
  "include": ["src"]
}
```

`promo/remotion.config.ts`：

```ts
import { Config } from '@remotion/cli/config';

Config.setVideoImageFormat('jpeg');
Config.setOverwriteOutput(true);
```

`promo/.gitignore`：

```
node_modules
out
```

- [ ] **Step 3: 主题 theme.ts**

`promo/src/theme.ts`：

```ts
import { loadFont } from '@remotion/google-fonts/NotoSansSC';

export const { fontFamily } = loadFont();

export const FPS = 30;
export const TOTAL = 1800;

export const COLORS = {
  bg0: '#0b1020',
  bg1: '#111827',
  up: '#c0392b',
  down: '#27ae60',
  accent: '#3b82f6',
  text: '#e5e7eb',
  sub: '#94a3b8',
  gold: '#facc15',
};

export type SceneDef = { name: string; durationInFrames: number };

export const SCENES: SceneDef[] = [
  { name: 'S1Hook', durationInFrames: 210 },
  { name: 'S2Logo', durationInFrames: 210 },
  { name: 'S3Backtest', durationInFrames: 300 },
  { name: 'S4Diagnose', durationInFrames: 270 },
  { name: 'S5Picks', durationInFrames: 300 },
  { name: 'S6Push', durationInFrames: 240 },
  { name: 'S7History', durationInFrames: 180 },
  { name: 'S8Cta', durationInFrames: 90 },
];
```

- [ ] **Step 4: 背景与字幕组件**

`promo/src/components/Bg.tsx`：

```tsx
import { AbsoluteFill } from 'remotion';
import { COLORS } from '../theme';

export const Bg: React.FC = () => (
  <AbsoluteFill
    style={{
      background: `radial-gradient(1200px 1200px at 50% 30%, ${COLORS.bg1}, ${COLORS.bg0})`,
    }}
  >
    <AbsoluteFill
      style={{
        backgroundImage:
          'linear-gradient(rgba(255,255,255,0.04) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.04) 1px, transparent 1px)',
        backgroundSize: '64px 64px',
        opacity: 0.6,
      }}
    />
  </AbsoluteFill>
);
```

`promo/src/components/Caption.tsx`：

```tsx
import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

export const Caption: React.FC<{ text: string; sub?: string; color?: string }> = ({
  text,
  sub,
  color = COLORS.text,
}) => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const s = spring({ frame, fps, config: { damping: 200 } });
  const y = interpolate(s, [0, 1], [40, 0]);
  const opacity = interpolate(frame, [0, 12], [0, 1], { extrapolateRight: 'clamp' });
  const size = vertical ? Math.round(width * 0.075) : Math.round(width * 0.045);
  return (
    <div
      style={{
        position: 'absolute',
        left: 0,
        right: 0,
        bottom: vertical ? '14%' : '12%',
        padding: '0 8%',
        textAlign: 'center',
        fontFamily,
        transform: `translateY(${y}px)`,
        opacity,
      }}
    >
      <div style={{ color, fontSize: size, fontWeight: 800, lineHeight: 1.25, textShadow: '0 2px 24px rgba(0,0,0,0.6)' }}>
        {text}
      </div>
      {sub ? (
        <div style={{ color: COLORS.sub, fontSize: size * 0.5, marginTop: 12, fontWeight: 500 }}>{sub}</div>
      ) : null}
    </div>
  );
};
```

- [ ] **Step 5: 音轨占位 AudioTrack.tsx**

`promo/src/components/AudioTrack.tsx`：

```tsx
// 音轨槽：默认关闭（缺音频文件会导致渲染失败）。
// 启用：把 narration.mp3 / bgm.mp3 放入 promo/public/，取消下面注释。
//
// import { Audio, staticFile } from 'remotion';
// export const AudioTrack: React.FC = () => (
//   <>
//     <Audio src={staticFile('narration.mp3')} />
//     <Audio src={staticFile('bgm.mp3')} volume={0.15} />
//   </>
// );

export const AudioTrack: React.FC = () => null;
```

- [ ] **Step 6: 8 个占位场景**

对 `S1Hook`…`S8Cta` 各建一个占位文件（后续任务替换其内容）。示例 `promo/src/scenes/S1Hook.tsx`：

```tsx
import { AbsoluteFill } from 'remotion';
import { Caption } from '../components/Caption';

export const S1Hook: React.FC = () => (
  <AbsoluteFill>
    <Caption text="S1Hook 占位" />
  </AbsoluteFill>
);
```

其余 7 个同构，仅改文件名、导出名与占位文字：
`S2Logo.tsx`(export `S2Logo`)、`S3Backtest.tsx`(`S3Backtest`)、`S4Diagnose.tsx`(`S4Diagnose`)、`S5Picks.tsx`(`S5Picks`)、`S6Push.tsx`(`S6Push`)、`S7History.tsx`(`S7History`)、`S8Cta.tsx`(`S8Cta`)。

- [ ] **Step 7: 主装配 Promo.tsx**

`promo/src/Promo.tsx`：

```tsx
import { AbsoluteFill, Series } from 'remotion';
import { SCENES } from './theme';
import { Bg } from './components/Bg';
import { AudioTrack } from './components/AudioTrack';
import { S1Hook } from './scenes/S1Hook';
import { S2Logo } from './scenes/S2Logo';
import { S3Backtest } from './scenes/S3Backtest';
import { S4Diagnose } from './scenes/S4Diagnose';
import { S5Picks } from './scenes/S5Picks';
import { S6Push } from './scenes/S6Push';
import { S7History } from './scenes/S7History';
import { S8Cta } from './scenes/S8Cta';

const MAP: Record<string, React.FC> = {
  S1Hook, S2Logo, S3Backtest, S4Diagnose, S5Picks, S6Push, S7History, S8Cta,
};

export const Promo: React.FC = () => (
  <AbsoluteFill>
    <Bg />
    <Series>
      {SCENES.map((s) => {
        const C = MAP[s.name];
        return (
          <Series.Sequence key={s.name} durationInFrames={s.durationInFrames}>
            <C />
          </Series.Sequence>
        );
      })}
    </Series>
    <AudioTrack />
  </AbsoluteFill>
);
```

- [ ] **Step 8: Root.tsx + index.ts**

`promo/src/Root.tsx`：

```tsx
import { Composition } from 'remotion';
import { Promo } from './Promo';
import { FPS, TOTAL } from './theme';

export const Root: React.FC = () => (
  <>
    <Composition id="PromoVertical" component={Promo} durationInFrames={TOTAL} fps={FPS} width={1080} height={1920} />
    <Composition id="PromoWide" component={Promo} durationInFrames={TOTAL} fps={FPS} width={1920} height={1080} />
  </>
);
```

`promo/src/index.ts`：

```ts
import { registerRoot } from 'remotion';
import { Root } from './Root';

registerRoot(Root);
```

- [ ] **Step 9: 安装、类型检查、渲染验证**

Run:
```bash
cd promo && npm install && npx tsc --noEmit
```
Expected: 安装成功，tsc 无错误。

Run（抽帧验证，首次会下载无头 Chrome，较慢；若离线/无法启动则记录并以 tsc 通过为准）:
```bash
mkdir -p out && npx remotion still src/index.ts PromoVertical out/preview-skeleton.png --frame=100
```
Expected: 生成 `out/preview-skeleton.png`（占位字幕可见）。

- [ ] **Step 10: Commit**

```bash
cd .. && git add promo .gitignore && git commit -m "feat(promo): Remotion 工程脚手架与骨架渲染"
```

---

### Task 2: 场景 S1 痛点钩子 + S2 系统亮相

**Files:**
- Create: `promo/src/components/Candles.tsx`
- Modify: `promo/src/scenes/S1Hook.tsx`、`promo/src/scenes/S2Logo.tsx`

**Interfaces:**
- Consumes：`Caption`、`COLORS`、`fontFamily`、`Bg`。
- Produces：`Candles`（K线闪烁）；实装 `S1Hook`、`S2Logo`。

- [ ] **Step 1: Candles 组件（K线随机涨跌）**

`promo/src/components/Candles.tsx`：

```tsx
import { interpolate, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS } from '../theme';

// 伪随机（确定性，随帧抖动）
const rnd = (i: number, f: number) => {
  const x = Math.sin(i * 12.9898 + f * 0.13) * 43758.5453;
  return x - Math.floor(x);
};

export const Candles: React.FC<{ count?: number }> = ({ count = 16 }) => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const n = vertical ? Math.min(count, 12) : count;
  const midY = height * (vertical ? 0.42 : 0.5);
  return (
    <>
      {Array.from({ length: n }).map((_, i) => {
        const up = rnd(i, Math.floor(frame / 6)) > 0.5;
        const h = 60 + rnd(i + 7, Math.floor(frame / 5)) * (vertical ? 260 : 200);
        const wick = h + 30 + rnd(i + 3, frame) * 40;
        const colW = (width * 0.9) / n;
        const x = width * 0.05 + i * colW + colW * 0.25;
        const color = up ? COLORS.up : COLORS.down;
        const jitter = interpolate(rnd(i, frame), [0, 1], [-14, 14]);
        return (
          <div key={i} style={{ position: 'absolute', left: x, top: midY - h / 2 + jitter, width: colW * 0.5 }}>
            <div style={{ position: 'absolute', left: '45%', top: -(wick - h) / 2, width: 2, height: wick, background: color, opacity: 0.5 }} />
            <div style={{ width: '100%', height: h, background: color, borderRadius: 3, opacity: 0.9 }} />
          </div>
        );
      })}
    </>
  );
};
```

- [ ] **Step 2: 实装 S1Hook**

`promo/src/scenes/S1Hook.tsx`：

```tsx
import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from 'remotion';
import { Candles } from '../components/Candles';
import { Caption } from '../components/Caption';
import { COLORS, fontFamily } from '../theme';

export const S1Hook: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const qOpacity = interpolate(frame, [30, 60], [0, 1], { extrapolateRight: 'clamp' });
  const qSize = vertical ? width * 0.26 : width * 0.16;
  return (
    <AbsoluteFill>
      <Candles />
      <div
        style={{
          position: 'absolute',
          top: vertical ? '30%' : '24%',
          left: 0,
          right: 0,
          textAlign: 'center',
          fontFamily,
          fontSize: qSize,
          fontWeight: 900,
          color: COLORS.gold,
          opacity: qOpacity,
          textShadow: '0 0 40px rgba(250,204,21,0.5)',
        }}
      >
        ?
      </div>
      <Caption text="追涨杀跌？该加仓还是减仓？" sub="凭感觉，其实就是在赌" color={COLORS.text} />
    </AbsoluteFill>
  );
};
```

- [ ] **Step 3: 实装 S2Logo**

`promo/src/scenes/S2Logo.tsx`：

```tsx
import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { COLORS, fontFamily } from '../theme';

export const S2Logo: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const pop = spring({ frame, fps, config: { damping: 12, stiffness: 120 } });
  const scale = interpolate(pop, [0, 1], [0.4, 1]);
  const glow = interpolate(frame % 90, [0, 45, 90], [0.3, 0.8, 0.3]);
  const logoSize = vertical ? width * 0.34 : width * 0.2;
  return (
    <AbsoluteFill style={{ justifyContent: 'center', alignItems: 'center' }}>
      <div
        style={{
          fontFamily,
          fontSize: logoSize,
          fontWeight: 900,
          letterSpacing: 4,
          color: COLORS.text,
          transform: `scale(${scale})`,
          textShadow: `0 0 ${40 * glow}px ${COLORS.accent}`,
        }}
      >
        xlh
      </div>
      <div
        style={{
          marginTop: 24,
          fontFamily,
          fontSize: (vertical ? width * 0.055 : width * 0.032),
          fontWeight: 700,
          color: COLORS.accent,
          opacity: interpolate(frame, [20, 45], [0, 1], { extrapolateRight: 'clamp' }),
        }}
      >
        投资研判系统
      </div>
      <Caption text="让每一个买卖决策，有数据撑腰" />
    </AbsoluteFill>
  );
};
```

- [ ] **Step 4: 验证**

Run:
```bash
cd promo && npx tsc --noEmit
```
Expected: 无错误。

Run（抽帧，best-effort）:
```bash
npx remotion still src/index.ts PromoVertical out/preview-s1.png --frame=120 && npx remotion still src/index.ts PromoVertical out/preview-s2.png --frame=320
```
Expected: 两张 PNG 生成，S1 见 K线+问号+字幕，S2 见 logo。

- [ ] **Step 5: Commit**

```bash
cd .. && git add promo && git commit -m "feat(promo): S1 痛点钩子 + S2 系统亮相"
```

---

### Task 3: 场景 S3 回测 + 参数寻优

**Files:**
- Create: `promo/src/components/GrowLine.tsx`、`promo/src/components/ParamGrid.tsx`
- Modify: `promo/src/scenes/S3Backtest.tsx`

**Interfaces:**
- Consumes：`Caption`、`COLORS`、`fontFamily`。
- Produces：`GrowLine`（收益曲线随帧生长）、`ParamGrid`（参数网格点亮锁定）；实装 `S3Backtest`。

- [ ] **Step 1: GrowLine**

`promo/src/components/GrowLine.tsx`：

```tsx
import { interpolate, useCurrentFrame } from 'remotion';
import { COLORS } from '../theme';

// 生成一条上行波动折线，随帧从左向右揭示。
export const GrowLine: React.FC<{ w: number; h: number }> = ({ w, h }) => {
  const frame = useCurrentFrame();
  const pts = 60;
  const path = Array.from({ length: pts }).map((_, i) => {
    const t = i / (pts - 1);
    const up = t * 0.8; // 总体上行
    const wave = Math.sin(i * 0.7) * 0.06 + Math.sin(i * 0.23) * 0.04;
    const y = h - (0.1 + up + wave) * h;
    return [t * w, y] as const;
  });
  const d = path.map((p, i) => `${i === 0 ? 'M' : 'L'}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(' ');
  const reveal = interpolate(frame, [10, 150], [0, 1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' });
  const dash = w * 1.6;
  return (
    <svg width={w} height={h} style={{ overflow: 'visible' }}>
      <defs>
        <linearGradient id="gl" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={COLORS.up} stopOpacity="0.35" />
          <stop offset="100%" stopColor={COLORS.up} stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d={`${d} L${w},${h} L0,${h} Z`} fill="url(#gl)" opacity={reveal} />
      <path
        d={d}
        fill="none"
        stroke={COLORS.up}
        strokeWidth={6}
        strokeLinecap="round"
        strokeDasharray={dash}
        strokeDashoffset={dash * (1 - reveal)}
      />
    </svg>
  );
};
```

- [ ] **Step 2: ParamGrid**

`promo/src/components/ParamGrid.tsx`：

```tsx
import { interpolate, useCurrentFrame } from 'remotion';
import { COLORS } from '../theme';

// 参数网格逐格点亮，最后锁定"最优"格（右下角）。
export const ParamGrid: React.FC<{ size: number }> = ({ size }) => {
  const frame = useCurrentFrame();
  const cols = 5;
  const rows = 5;
  const cell = size / cols;
  const best = { r: 3, c: 4 };
  return (
    <svg width={size} height={cell * rows}>
      {Array.from({ length: rows }).map((_, r) =>
        Array.from({ length: cols }).map((__, c) => {
          const idx = r * cols + c;
          const on = interpolate(frame, [60 + idx * 4, 72 + idx * 4], [0, 1], {
            extrapolateLeft: 'clamp',
            extrapolateRight: 'clamp',
          });
          const isBest = r === best.r && c === best.c;
          const lock = isBest ? interpolate(frame, [200, 230], [0, 1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }) : 0;
          const heat = 0.15 + on * 0.6;
          return (
            <rect
              key={`${r}-${c}`}
              x={c * cell + 4}
              y={r * cell + 4}
              width={cell - 8}
              height={cell - 8}
              rx={8}
              fill={isBest ? COLORS.gold : COLORS.accent}
              opacity={isBest ? 0.3 + lock * 0.7 : heat}
              stroke={isBest && lock > 0.2 ? COLORS.gold : 'transparent'}
              strokeWidth={4}
            />
          );
        })
      )}
    </svg>
  );
};
```

- [ ] **Step 3: 实装 S3Backtest**

`promo/src/scenes/S3Backtest.tsx`：

```tsx
import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { GrowLine } from '../components/GrowLine';
import { ParamGrid } from '../components/ParamGrid';

export const S3Backtest: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const lineW = vertical ? width * 0.86 : width * 0.5;
  const lineH = vertical ? height * 0.28 : height * 0.42;
  const gridSize = vertical ? width * 0.55 : width * 0.28;
  return (
    <AbsoluteFill
      style={{
        flexDirection: vertical ? 'column' : 'row',
        justifyContent: 'center',
        alignItems: 'center',
        gap: vertical ? 40 : 80,
        paddingTop: vertical ? '12%' : 0,
      }}
    >
      <GrowLine w={lineW} h={lineH} />
      <ParamGrid size={gridSize} />
      <Caption text="定投/择时回测 · 参数寻优" sub="策略有没有效，先用历史验证" />
    </AbsoluteFill>
  );
};
```

- [ ] **Step 4: 验证**

Run: `cd promo && npx tsc --noEmit`
Expected: 无错误。
Run（best-effort）: `npx remotion still src/index.ts PromoVertical out/preview-s3.png --frame=560`
Expected: 见收益曲线 + 参数网格（最优格金色高亮）+ 字幕。

- [ ] **Step 5: Commit**

```bash
cd .. && git add promo && git commit -m "feat(promo): S3 回测与参数寻优"
```

---

### Task 4: 场景 S4 市场诊断 + 多市场覆盖

**Files:**
- Create: `promo/src/components/StatusLight.tsx`、`promo/src/components/MarketTags.tsx`
- Modify: `promo/src/scenes/S4Diagnose.tsx`

**Interfaces:**
- Consumes：`Caption`、`COLORS`、`fontFamily`。
- Produces：`StatusLight`（三色状态灯切换）、`MarketTags`（A股/港股/美股标签浮现）；实装 `S4Diagnose`。

- [ ] **Step 1: StatusLight**

`promo/src/components/StatusLight.tsx`：

```tsx
import { interpolate, useCurrentFrame } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const STATES = [
  { label: '上涨趋势', color: COLORS.up },
  { label: '震荡', color: COLORS.sub },
  { label: '下跌趋势', color: COLORS.down },
];

export const StatusLight: React.FC<{ size: number }> = ({ size }) => {
  const frame = useCurrentFrame();
  const active = Math.min(STATES.length - 1, Math.floor(frame / 45) % STATES.length);
  return (
    <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 20 }}>
      <div style={{ display: 'flex', gap: 28 }}>
        {STATES.map((s, i) => {
          const on = i === active ? 1 : 0.2;
          const pulse = i === active ? interpolate(frame % 45, [0, 22, 45], [0.6, 1, 0.6]) : 0.2;
          return (
            <div
              key={s.label}
              style={{
                width: size,
                height: size,
                borderRadius: '50%',
                background: s.color,
                opacity: on,
                boxShadow: i === active ? `0 0 ${size * 0.8}px ${s.color}` : 'none',
                transform: `scale(${0.9 + pulse * 0.2})`,
              }}
            />
          );
        })}
      </div>
      <div style={{ fontFamily, fontSize: size * 0.9, fontWeight: 800, color: STATES[active].color }}>
        {STATES[active].label}
      </div>
    </div>
  );
};
```

- [ ] **Step 2: MarketTags**

`promo/src/components/MarketTags.tsx`：

```tsx
import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const TAGS = ['A股', '港股', '美股'];

export const MarketTags: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const fs = vertical ? width * 0.06 : width * 0.032;
  return (
    <div style={{ display: 'flex', gap: 20 }}>
      {TAGS.map((t, i) => {
        const s = spring({ frame: frame - (90 + i * 18), fps, config: { damping: 14 } });
        const y = interpolate(s, [0, 1], [30, 0]);
        return (
          <div
            key={t}
            style={{
              fontFamily,
              fontSize: fs,
              fontWeight: 800,
              color: COLORS.text,
              padding: '10px 24px',
              borderRadius: 999,
              border: `2px solid ${COLORS.accent}`,
              background: 'rgba(59,130,246,0.12)',
              opacity: Math.max(0, s),
              transform: `translateY(${y}px)`,
            }}
          >
            {t}
          </div>
        );
      })}
    </div>
  );
};
```

- [ ] **Step 3: 实装 S4Diagnose**

`promo/src/scenes/S4Diagnose.tsx`：

```tsx
import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { StatusLight } from '../components/StatusLight';
import { MarketTags } from '../components/MarketTags';

export const S4Diagnose: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const dot = vertical ? width * 0.12 : width * 0.06;
  return (
    <AbsoluteFill
      style={{
        flexDirection: 'column',
        justifyContent: 'center',
        alignItems: 'center',
        gap: vertical ? 60 : 48,
        paddingBottom: vertical ? '10%' : '6%',
      }}
    >
      <StatusLight size={dot} />
      <MarketTags />
      <Caption text="市场状态诊断" sub="A股 / 港股 / 美股 全覆盖" />
    </AbsoluteFill>
  );
};
```

- [ ] **Step 4: 验证**

Run: `cd promo && npx tsc --noEmit`
Expected: 无错误。
Run（best-effort）: `npx remotion still src/index.ts PromoVertical out/preview-s4.png --frame=850`
Expected: 见状态灯 + A股/港股/美股标签 + 字幕。

- [ ] **Step 5: Commit**

```bash
cd .. && git add promo && git commit -m "feat(promo): S4 市场诊断与多市场覆盖"
```

---

### Task 5: 场景 S5 选股 + 持仓建议

**Files:**
- Create: `promo/src/components/StockCard.tsx`、`promo/src/components/HoldingRow.tsx`
- Modify: `promo/src/scenes/S5Picks.tsx`

**Interfaces:**
- Consumes：`Caption`、`COLORS`、`fontFamily`。
- Produces：`StockCard({rank,name,score,delay})`、`HoldingRow({name,action,amount,delay})`；实装 `S5Picks`。动作颜色：加仓/止盈=up 红，减仓=down 绿，持有=sub。

- [ ] **Step 1: StockCard**

`promo/src/components/StockCard.tsx`：

```tsx
import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

export const StockCard: React.FC<{ rank: number; name: string; score: number; delay: number }> = ({
  rank,
  name,
  score,
  delay,
}) => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const s = spring({ frame: frame - delay, fps, config: { damping: 16 } });
  const x = interpolate(s, [0, 1], [-60, 0]);
  const fs = vertical ? width * 0.045 : width * 0.024;
  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 18,
        padding: '14px 22px',
        borderRadius: 14,
        background: 'rgba(255,255,255,0.05)',
        border: '1px solid rgba(255,255,255,0.08)',
        fontFamily,
        opacity: Math.max(0, s),
        transform: `translateX(${x}px)`,
        width: vertical ? '80%' : 420,
      }}
    >
      <div style={{ fontSize: fs * 1.1, fontWeight: 900, color: COLORS.gold, width: fs * 1.4 }}>#{rank}</div>
      <div style={{ fontSize: fs, fontWeight: 700, color: COLORS.text, flex: 1 }}>{name}</div>
      <div style={{ fontSize: fs * 0.9, fontWeight: 800, color: COLORS.accent }}>{score.toFixed(1)}</div>
    </div>
  );
};
```

- [ ] **Step 2: HoldingRow**

`promo/src/components/HoldingRow.tsx`：

```tsx
import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const actionColor = (a: string) =>
  a === '加仓' || a === '止盈' ? COLORS.up : a === '减仓' ? COLORS.down : COLORS.sub;

export const HoldingRow: React.FC<{ name: string; action: string; amount: string; delay: number }> = ({
  name,
  action,
  amount,
  delay,
}) => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const s = spring({ frame: frame - delay, fps, config: { damping: 16 } });
  const fs = vertical ? width * 0.042 : width * 0.022;
  const tag = interpolate(s, [0, 1], [0.6, 1]);
  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 16,
        padding: '12px 20px',
        borderRadius: 12,
        background: 'rgba(255,255,255,0.04)',
        fontFamily,
        opacity: Math.max(0, s),
        width: vertical ? '80%' : 460,
      }}
    >
      <div style={{ fontSize: fs, fontWeight: 700, color: COLORS.text, flex: 1 }}>{name}</div>
      <div
        style={{
          fontSize: fs * 0.85,
          fontWeight: 800,
          color: '#fff',
          background: actionColor(action),
          padding: '4px 14px',
          borderRadius: 999,
          transform: `scale(${tag})`,
        }}
      >
        {action}
      </div>
      <div style={{ fontSize: fs * 0.8, color: COLORS.sub, width: fs * 4, textAlign: 'right' }}>{amount}</div>
    </div>
  );
};
```

- [ ] **Step 3: 实装 S5Picks**

`promo/src/scenes/S5Picks.tsx`：

```tsx
import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { StockCard } from '../components/StockCard';
import { HoldingRow } from '../components/HoldingRow';

const PICKS = [
  { rank: 1, name: '贵州茅台', score: 9.2 },
  { rank: 2, name: '宁德时代', score: 8.7 },
  { rank: 3, name: '腾讯控股', score: 8.1 },
];
const HOLDINGS = [
  { name: '沪深300ETF', action: '加仓', amount: '+2,000' },
  { name: '中概互联', action: '持有', amount: '—' },
  { name: '白酒LOF', action: '止盈', amount: '-1,500' },
];

export const S5Picks: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  return (
    <AbsoluteFill
      style={{
        flexDirection: vertical ? 'column' : 'row',
        justifyContent: 'center',
        alignItems: 'center',
        gap: vertical ? 30 : 60,
        paddingTop: vertical ? '8%' : 0,
      }}
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: 14, alignItems: 'center' }}>
        {PICKS.map((p, i) => (
          <StockCard key={p.name} {...p} delay={20 + i * 14} />
        ))}
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 12, alignItems: 'center' }}>
        {HOLDINGS.map((h, i) => (
          <HoldingRow key={h.name} {...h} delay={110 + i * 16} />
        ))}
      </div>
      <Caption text="跨股选股 · 逐只持仓建议" sub="加仓 / 持有 / 减仓 / 止盈 + 建议金额" />
    </AbsoluteFill>
  );
};
```

- [ ] **Step 4: 验证**

Run: `cd promo && npx tsc --noEmit`
Expected: 无错误。
Run（best-effort）: `npx remotion still src/index.ts PromoVertical out/preview-s5.png --frame=1200`
Expected: 见候选股排名卡 + 持仓行(动作标签配色正确) + 字幕。

- [ ] **Step 5: Commit**

```bash
cd .. && git add promo && git commit -m "feat(promo): S5 选股与持仓建议"
```

---

### Task 6: 场景 S6 自动推送 + S7 历史/可控

**Files:**
- Create: `promo/src/components/PhoneNotify.tsx`、`promo/src/components/Timeline.tsx`
- Modify: `promo/src/scenes/S6Push.tsx`、`promo/src/scenes/S7History.tsx`

**Interfaces:**
- Consumes：`Caption`、`COLORS`、`fontFamily`。
- Produces：`PhoneNotify`（手机+消息卡片依次滑入）、`Timeline`（历史时间线滑动+盾牌）；实装 `S6Push`、`S7History`。

- [ ] **Step 1: PhoneNotify**

`promo/src/components/PhoneNotify.tsx`：

```tsx
import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const MSGS = [
  { app: '微信', text: '持仓建议已更新：沪深300 建议加仓' },
  { app: '钉钉', text: '基金诊断：当前震荡，注意仓位' },
  { app: '飞书', text: '每日推送 · 3 只持仓 2 加 1 止盈' },
];

export const PhoneNotify: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const phoneW = vertical ? width * 0.6 : height * 0.62;
  const phoneH = phoneW * 2.05;
  const fs = phoneW * 0.062;
  return (
    <div
      style={{
        width: phoneW,
        height: phoneH,
        borderRadius: phoneW * 0.12,
        border: `${phoneW * 0.02}px solid #333`,
        background: '#05070f',
        padding: phoneW * 0.06,
        display: 'flex',
        flexDirection: 'column',
        gap: phoneW * 0.045,
        boxShadow: '0 20px 80px rgba(0,0,0,0.6)',
      }}
    >
      {MSGS.map((m, i) => {
        const s = spring({ frame: frame - (20 + i * 34), fps, config: { damping: 16 } });
        const y = interpolate(s, [0, 1], [-40, 0]);
        return (
          <div
            key={m.app}
            style={{
              background: 'rgba(255,255,255,0.06)',
              borderRadius: phoneW * 0.05,
              padding: phoneW * 0.05,
              fontFamily,
              opacity: Math.max(0, s),
              transform: `translateY(${y}px)`,
            }}
          >
            <div style={{ color: COLORS.accent, fontSize: fs * 0.8, fontWeight: 800 }}>{m.app}</div>
            <div style={{ color: COLORS.text, fontSize: fs, fontWeight: 600, marginTop: 6, lineHeight: 1.3 }}>{m.text}</div>
          </div>
        );
      })}
    </div>
  );
};
```

- [ ] **Step 2: Timeline**

`promo/src/components/Timeline.tsx`：

```tsx
import { interpolate, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const DATES = ['06-28', '07-01', '07-03', '07-05'];

export const Timeline: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const fs = vertical ? width * 0.04 : width * 0.022;
  const slide = interpolate(frame, [0, 60], [40, 0], { extrapolateRight: 'clamp' });
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: vertical ? 18 : 30, transform: `translateX(${slide}px)`, fontFamily }}>
      {DATES.map((d, i) => {
        const on = interpolate(frame, [i * 16, i * 16 + 16], [0, 1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' });
        return (
          <div key={d} style={{ display: 'flex', alignItems: 'center', gap: vertical ? 18 : 30 }}>
            <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 8, opacity: 0.4 + on * 0.6 }}>
              <div style={{ width: fs * 0.7, height: fs * 0.7, borderRadius: '50%', background: COLORS.accent }} />
              <div style={{ color: COLORS.sub, fontSize: fs * 0.7 }}>{d}</div>
            </div>
            {i < DATES.length - 1 ? <div style={{ width: vertical ? 30 : 60, height: 3, background: 'rgba(255,255,255,0.2)' }} /> : null}
          </div>
        );
      })}
    </div>
  );
};
```

- [ ] **Step 3: 实装 S6Push**

`promo/src/scenes/S6Push.tsx`：

```tsx
import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { PhoneNotify } from '../components/PhoneNotify';

export const S6Push: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  return (
    <AbsoluteFill style={{ justifyContent: 'center', alignItems: 'center', paddingTop: vertical ? '4%' : 0, paddingBottom: vertical ? '18%' : '8%' }}>
      <PhoneNotify />
      <Caption text="定时推送到微信 / 钉钉 / 飞书" sub="不用盯盘，建议自动送达" />
    </AbsoluteFill>
  );
};
```

- [ ] **Step 4: 实装 S7History**

`promo/src/scenes/S7History.tsx`：

```tsx
import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { Timeline } from '../components/Timeline';
import { COLORS, fontFamily } from '../theme';

export const S7History: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const shield = interpolate(frame, [50, 80], [0, 1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' });
  const fs = vertical ? width * 0.05 : width * 0.028;
  return (
    <AbsoluteFill style={{ flexDirection: 'column', justifyContent: 'center', alignItems: 'center', gap: 50, paddingBottom: vertical ? '10%' : '6%' }}>
      <Timeline />
      <div style={{ display: 'flex', alignItems: 'center', gap: 14, opacity: shield, fontFamily }}>
        <span style={{ fontSize: fs * 1.4 }}>🛡️</span>
        <span style={{ color: COLORS.text, fontSize: fs, fontWeight: 700 }}>本地 / 私有部署，数据自己掌控</span>
      </div>
      <Caption text="持仓建议历史，决策可回溯" />
    </AbsoluteFill>
  );
};
```

- [ ] **Step 5: 验证**

Run: `cd promo && npx tsc --noEmit`
Expected: 无错误。
Run（best-effort）: `npx remotion still src/index.ts PromoVertical out/preview-s6.png --frame=1420 && npx remotion still src/index.ts PromoVertical out/preview-s7.png --frame=1620`
Expected: S6 见手机+3 条消息，S7 见时间线+盾牌+字幕。

- [ ] **Step 6: Commit**

```bash
cd .. && git add promo && git commit -m "feat(promo): S6 自动推送 + S7 历史与私有可控"
```

---

### Task 7: 场景 S8 CTA + 全片渲染验证

**Files:**
- Create: `promo/src/components/Bullets.tsx`、`promo/src/components/QRPlaceholder.tsx`
- Modify: `promo/src/scenes/S8Cta.tsx`

**Interfaces:**
- Consumes：`COLORS`、`fontFamily`。
- Produces：`Bullets`（优势 icon 列）、`QRPlaceholder`（二维码占位+微信号）；实装 `S8Cta`。

- [ ] **Step 1: Bullets**

`promo/src/components/Bullets.tsx`：

```tsx
import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const ITEMS = [
  { icon: '📊', label: '数据驱动' },
  { icon: '🌏', label: '多市场' },
  { icon: '🔔', label: '自动省心' },
  { icon: '🛡️', label: '轻量私有' },
];

export const Bullets: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const fs = vertical ? width * 0.04 : width * 0.022;
  return (
    <div style={{ display: 'flex', gap: vertical ? 18 : 36, flexWrap: 'wrap', justifyContent: 'center', maxWidth: '92%' }}>
      {ITEMS.map((it, i) => {
        const s = spring({ frame: frame - i * 6, fps, config: { damping: 14 } });
        return (
          <div
            key={it.label}
            style={{
              display: 'flex',
              flexDirection: 'column',
              alignItems: 'center',
              gap: 8,
              fontFamily,
              opacity: Math.max(0, s),
              transform: `scale(${interpolate(s, [0, 1], [0.6, 1])})`,
            }}
          >
            <div style={{ fontSize: fs * 1.6 }}>{it.icon}</div>
            <div style={{ color: COLORS.text, fontSize: fs * 0.8, fontWeight: 700 }}>{it.label}</div>
          </div>
        );
      })}
    </div>
  );
};
```

- [ ] **Step 2: QRPlaceholder**

`promo/src/components/QRPlaceholder.tsx`：

```tsx
import { useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

// 二维码占位：把真实二维码放 promo/public/qr.png 后，可改为 <Img src={staticFile('qr.png')} />。
export const QRPlaceholder: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const box = vertical ? width * 0.28 : width * 0.14;
  const fs = vertical ? width * 0.05 : width * 0.028;
  return (
    <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 14, fontFamily }}>
      <div
        style={{
          width: box,
          height: box,
          background: '#fff',
          borderRadius: 12,
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          color: '#111',
          fontSize: fs * 0.5,
          fontWeight: 700,
        }}
      >
        二维码
      </div>
      <div style={{ color: COLORS.gold, fontSize: fs, fontWeight: 900 }}>微信 I1346535693</div>
    </div>
  );
};
```

- [ ] **Step 3: 实装 S8Cta**

`promo/src/scenes/S8Cta.tsx`：

```tsx
import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from 'remotion';
import { Bullets } from '../components/Bullets';
import { QRPlaceholder } from '../components/QRPlaceholder';
import { COLORS, fontFamily } from '../theme';

export const S8Cta: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const titleOp = interpolate(frame, [0, 15], [0, 1], { extrapolateRight: 'clamp' });
  const fs = vertical ? width * 0.06 : width * 0.034;
  return (
    <AbsoluteFill style={{ flexDirection: 'column', justifyContent: 'center', alignItems: 'center', gap: vertical ? 46 : 40 }}>
      <div style={{ fontFamily, fontSize: fs, fontWeight: 900, color: COLORS.text, opacity: titleOp }}>
        注册授权，即刻上手
      </div>
      <Bullets />
      <QRPlaceholder />
    </AbsoluteFill>
  );
};
```

- [ ] **Step 4: 类型检查 + 全片渲染（两版）**

Run: `cd promo && npx tsc --noEmit`
Expected: 无错误。

Run（完整渲染，作为验收；首次下载无头 Chrome 较慢；若离线无法渲染，记录并至少确保 tsc 通过、`npx remotion still` 对 S8 单帧可出图）:
```bash
npm run render:v && npm run render:w
```
Expected: 生成 `out/xlh-9x16.mp4`（1080×1920）与 `out/xlh-16x9.mp4`（1920×1080），各约 60s、含全部 8 场景。

- [ ] **Step 5: Commit**

```bash
cd .. && git add promo && git commit -m "feat(promo): S8 CTA 与全片渲染"
```

---

### Task 8: 文案包与说明文档

**Files:**
- Create: `promo/COPY.md`、`promo/README.md`

**Interfaces:** 无代码接口，纯文档。

- [ ] **Step 1: 文案包 COPY.md**

创建 `promo/COPY.md`，内容包含三部分：

1. **配音口播稿（8 段）**，逐场景抄录 spec §3 的 VO 列：
   - S1：追涨杀跌，该加还是该减？凭感觉，其实就是在赌。
   - S2：认识一下 xlh 投资研判系统——让每一个买卖决策，都有数据撑腰。
   - S3：定投还是择时？哪套参数最优？先用历史数据跑回测、自动寻优，用结果说话。
   - S4：当下是涨、是跌、还是震荡？一眼诊断。A股、港股、美股，全都覆盖。
   - S5：跨股票帮你选优，再对你的每一只持仓，给出加仓、持有、减仓还是止盈，连建议金额都算好。
   - S6：到点自动同步数据、生成建议，直接推送到你的微信、钉钉、飞书。不用天天盯盘。
   - S7：每次建议都存档，决策可回溯；本地私有部署，数据牢牢在你自己手里。
   - S8：数据驱动，多市场，自动省心。现在就上手——微信 I1346535693。

2. **各平台文案**：
   - **抖音/快手**：钩子（前3秒）「你买基金/股票，是靠感觉还是靠数据？」；正文一句话卖点 + 微信号；建议 15s/30s/60s 卡点说明。
   - **小红书**：标题「不再拍脑袋买卖！我用这套系统给持仓做体检📈」；正文（痛点→功能→优势→微信引导，200 字内）；话题标签。
   - **B站**：标题「A股/港股/美股一站搞定：回测+诊断+持仓建议+自动推送的投研系统」；简介（功能罗列 + 微信）；分区「科技/财经」，标签。

3. **话题标签池**：`#基金 #股票 #A股 #港股 #美股 #量化投资 #理财工具 #定投 #择时 #回测 #投资研判 #持仓建议`，注明各平台取 5–10 个。

- [ ] **Step 2: 说明文档 README.md**

创建 `promo/README.md`，内容：

- 简介：xlh 宣传片 Remotion 工程，输出 9:16 与 16:9 两版 MP4。
- 环境：Node ≥18。
- 命令：
  ```bash
  cd promo
  npm install
  npm run studio            # 预览/调参（http 本地）
  npm run render:v          # 竖屏 out/xlh-9x16.mp4
  npm run render:w          # 横屏 out/xlh-16x9.mp4
  ```
- 加音频：把 `narration.mp3` / `bgm.mp3` 放 `public/`，按 `src/components/AudioTrack.tsx` 注释启用后再渲染。
- 换二维码：把二维码图放 `public/qr.png`，改 `src/components/QRPlaceholder.tsx` 为 `<Img src={staticFile('qr.png')} />`。
- 改时长/节奏：编辑 `src/theme.ts` 的 `SCENES` 帧表与 `TOTAL`。
- 文案：见 `COPY.md`。

- [ ] **Step 3: Commit**

```bash
git add promo/COPY.md promo/README.md && git commit -m "docs(promo): 文案包与渲染说明"
```

---

## Self-Review

**1. Spec coverage：**
- Remotion 工程/双画幅/60s/8 场景 → Task 1（脚手架+SCENES+两 Composition）✅
- 8 场景内容（痛点/亮相/回测寻优/诊断多市场/选股持仓/推送/历史可控/CTA）→ Task 2–7 ✅
- 红涨绿跌配色、暗色、NotoSansSC → Task 1（theme/COLORS/fontFamily），全场景复用 ✅
- 自适应竖/横 → 各场景 `useVideoConfig()` + vertical 分支 ✅
- 音轨默认关 → Task 1 AudioTrack ✅
- CTA 微信 I1346535693 + 二维码占位 → Task 7（QRPlaceholder）✅
- 文案包（口播稿+各平台+话题）→ Task 8 COPY.md ✅
- 渲染说明/加音频/换二维码/改时长 → Task 8 README.md ✅
- 独立工程不碰 Rust、gitignore → Task 1（根 .gitignore 追加 promo/node_modules、out）✅
- 验收（两版 MP4 可渲染）→ Task 7 Step 4 ✅

**2. Placeholder scan：** `QRPlaceholder`/`AudioTrack` 为**有意占位**（spec 非目标声明），非 TODO 遗留；每步含完整代码。Task 1 的 8 个占位场景是分步接线（Task 2–7 替换），非空壳残留。

**3. Type consistency：**
- `SCENES` name 与 `Promo.tsx` 的 `MAP` 键、各 `scenes/SN.tsx` 导出名一致（S1Hook..S8Cta）。
- `theme.ts` 导出 `fontFamily/FPS/TOTAL/COLORS/SCENES` 全程一致引用。
- 组件 props 签名（`Caption{text,sub?,color?}`、`StockCard{rank,name,score,delay}`、`HoldingRow{name,action,amount,delay}`、`GrowLine{w,h}`、`ParamGrid{size}`、`StatusLight{size}`）在定义与场景调用处一致。
- 两 Composition 尺寸/帧率/时长与 Global Constraints 一致。

> 实现注意：Remotion 4 的 CLI 命令带入口 `src/index.ts`；渲染首次会下载无头 Chrome，需联网。若环境无法渲染，`npx tsc --noEmit` 通过即视为该任务代码就绪，渲染验收留待可联网环境。
