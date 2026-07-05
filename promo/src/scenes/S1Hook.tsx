import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from 'remotion';
import { Candles } from '../components/Candles';
import { Caption } from '../components/Caption';
import { COLORS, fontFamily } from '../theme';

export const S1Hook: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const qOpacity = interpolate(frame, [30, 60], [0, 1], { extrapolateRight: 'clamp' });
  const qSize = vertical ? width * 0.26 : width * 0.16;
  return (
    <AbsoluteFill>
      <Candles />
      <div
        style={{
          position: 'absolute',
          top: vertical ? '30%' : '24%',
          left: 0,
          right: 0,
          textAlign: 'center',
          fontFamily,
          fontSize: qSize,
          fontWeight: 900,
          color: COLORS.gold,
          opacity: qOpacity,
          textShadow: '0 0 40px rgba(250,204,21,0.5)',
        }}
      >
        ?
      </div>
      <Caption text="追涨杀跌？该加仓还是减仓？" sub="凭感觉买卖，很容易在情绪里追高杀跌、反复亏钱" color={COLORS.text} />
    </AbsoluteFill>
  );
};
