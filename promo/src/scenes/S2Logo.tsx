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
