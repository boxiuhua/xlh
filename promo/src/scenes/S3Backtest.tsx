import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { GrowLine } from '../components/GrowLine';
import { ParamGrid } from '../components/ParamGrid';

export const S3Backtest: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const lineW = vertical ? width * 0.86 : width * 0.5;
  const lineH = vertical ? height * 0.22 : height * 0.42;
  const gridSize = vertical ? width * 0.48 : width * 0.28;
  return (
    <AbsoluteFill
      style={{
        flexDirection: vertical ? 'column' : 'row',
        justifyContent: vertical ? 'flex-start' : 'center',
        alignItems: 'center',
        gap: vertical ? 40 : 80,
        paddingTop: vertical ? '10%' : 0,
      }}
    >
      <GrowLine w={lineW} h={lineH} />
      <ParamGrid size={gridSize} />
      <Caption text="定投/择时回测 · 参数寻优" sub="策略有没有效，先用历史验证" />
    </AbsoluteFill>
  );
};
