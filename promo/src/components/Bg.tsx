import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS } from '../theme';

// 漂浮发光球：随帧缓慢移动 + 呼吸明暗，营造鲜艳流动感。
const ORBS = [
  { c: COLORS.purple, x: 18, y: 20, r: 46, sx: 8, sy: 6, ph: 0 },
  { c: COLORS.cyan, x: 82, y: 30, r: 40, sx: -7, sy: 9, ph: 1.7 },
  { c: COLORS.pink, x: 30, y: 82, r: 44, sx: 9, sy: -6, ph: 3.1 },
  { c: COLORS.accent, x: 74, y: 74, r: 50, sx: -6, sy: -8, ph: 4.4 },
];

export const Bg: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();
  const t = frame / 30;
  const hue = interpolate(frame % 1200, [0, 1200], [0, 40]);
  return (
    <AbsoluteFill style={{ background: `radial-gradient(1400px 1400px at 50% 20%, ${COLORS.bg1}, ${COLORS.bg0})` }}>
      {/* 流动色相叠层 */}
      <AbsoluteFill style={{ background: `linear-gradient(${120 + hue}deg, rgba(168,85,247,0.16), rgba(34,211,238,0.10))`, mixBlendMode: 'screen' }} />
      {/* 发光球 */}
      {ORBS.map((o, i) => {
        const x = o.x + Math.sin(t * 0.25 + o.ph) * o.sx;
        const y = o.y + Math.cos(t * 0.22 + o.ph) * o.sy;
        const glow = interpolate(Math.sin(t * 0.6 + o.ph), [-1, 1], [0.35, 0.75]);
        const size = Math.min(width, height) * (o.r / 100);
        return (
          <div
            key={i}
            style={{
              position: 'absolute',
              left: `${x}%`,
              top: `${y}%`,
              width: size,
              height: size,
              marginLeft: -size / 2,
              marginTop: -size / 2,
              borderRadius: '50%',
              background: o.c,
              filter: `blur(${size * 0.42}px)`,
              opacity: glow * 0.5,
            }}
          />
        );
      })}
      {/* 细网格 */}
      <AbsoluteFill
        style={{
          backgroundImage:
            'linear-gradient(rgba(255,255,255,0.045) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.045) 1px, transparent 1px)',
          backgroundSize: '72px 72px',
          opacity: 0.5,
        }}
      />
    </AbsoluteFill>
  );
};
