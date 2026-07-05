import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily, GRAD } from '../theme';

export const Caption: React.FC<{ text: string; sub?: string; color?: string; warm?: boolean }> = ({
  text,
  sub,
  warm,
}) => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const s = spring({ frame, fps, config: { damping: 200 } });
  const y = interpolate(s, [0, 1], [46, 0]);
  const pop = spring({ frame, fps, config: { damping: 14, stiffness: 120 } });
  const scale = interpolate(pop, [0, 1], [0.9, 1]);
  const opacity = interpolate(frame, [0, 12], [0, 1], { extrapolateRight: 'clamp' });
  const size = vertical ? Math.round(width * 0.078) : Math.round(width * 0.046);
  const grad = warm ? GRAD.warm : GRAD.title;
  return (
    <div
      style={{
        position: 'absolute',
        left: 0,
        right: 0,
        bottom: vertical ? '13%' : '11%',
        padding: '0 7%',
        textAlign: 'center',
        fontFamily,
        transform: `translateY(${y}px) scale(${scale})`,
        opacity,
      }}
    >
      <div
        style={{
          fontSize: size,
          fontWeight: 900,
          lineHeight: 1.22,
          backgroundImage: grad,
          WebkitBackgroundClip: 'text',
          backgroundClip: 'text',
          WebkitTextFillColor: 'transparent',
          color: 'transparent',
          filter: `drop-shadow(0 0 22px ${warm ? 'rgba(255,146,60,0.5)' : 'rgba(79,140,255,0.5)'})`,
        }}
      >
        {text}
      </div>
      {sub ? (
        <div style={{ color: COLORS.sub, fontSize: size * 0.48, marginTop: 12, fontWeight: 600, textShadow: '0 2px 16px rgba(0,0,0,0.6)' }}>
          {sub}
        </div>
      ) : null}
    </div>
  );
};
