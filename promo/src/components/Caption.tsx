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
