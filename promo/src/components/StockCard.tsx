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
