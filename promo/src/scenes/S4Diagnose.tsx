import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { StatusLight } from '../components/StatusLight';
import { MarketTags } from '../components/MarketTags';

export const S4Diagnose: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const dot = vertical ? width * 0.12 : width * 0.06;
  return (
    <AbsoluteFill
      style={{
        flexDirection: 'column',
        justifyContent: 'center',
        alignItems: 'center',
        gap: vertical ? 60 : 48,
        paddingBottom: vertical ? '10%' : '6%',
      }}
    >
      <StatusLight size={dot} />
      <MarketTags />
      <Caption text="市场状态诊断" sub="A股 / 港股 / 美股 全覆盖" />
    </AbsoluteFill>
  );
};
