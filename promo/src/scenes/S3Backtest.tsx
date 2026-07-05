import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { Screenshot } from '../components/Screenshot';

export const S3Backtest: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const w = vertical ? width * 0.9 : width * 0.62;
  const h = vertical ? height * 0.44 : height * 0.5;
  return (
    <AbsoluteFill style={{ justifyContent: 'flex-start', alignItems: 'center', paddingTop: vertical ? '9%' : '6%' }}>
      <Screenshot src="shots/backtest.png" w={w} h={h} title="xlh · 基金回测报告" pan />
      <Caption text="定投/择时回测 · 参数寻优" sub="策略有没有效？先用历史跑回测——收益、年化、回撤、夏普一目了然" />
    </AbsoluteFill>
  );
};
