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
