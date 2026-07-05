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
