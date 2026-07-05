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
