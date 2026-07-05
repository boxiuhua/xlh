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
