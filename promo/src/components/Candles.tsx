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
