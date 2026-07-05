import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { Timeline } from '../components/Timeline';
import { COLORS, fontFamily } from '../theme';

export const S7History: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const shield = interpolate(frame, [50, 80], [0, 1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' });
  const fs = vertical ? width * 0.05 : width * 0.028;
  return (
    <AbsoluteFill style={{ flexDirection: 'column', justifyContent: 'center', alignItems: 'center', gap: 50, paddingBottom: vertical ? '10%' : '6%' }}>
      <Timeline />
      <div style={{ display: 'flex', alignItems: 'center', gap: 14, opacity: shield, fontFamily }}>
        <span style={{ fontSize: fs * 1.4 }}>🛡️</span>
        <span style={{ color: COLORS.text, fontSize: fs, fontWeight: 700 }}>本地 / 私有部署，数据自己掌控</span>
      </div>
      <Caption text="持仓建议历史，决策可回溯" sub="每次建议自动存档、随时回看；本地私有部署，数据自己掌控" />
    </AbsoluteFill>
  );
};
