import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { Screenshot } from '../components/Screenshot';

export const S5Picks: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const w = vertical ? width * 0.9 : width * 0.5;
  // holdings.png 为宽图(3200x2000)，窗口高按其比例，避免留白。
  const h = w * (2000 / 3200);
  return (
    <AbsoluteFill style={{ justifyContent: 'flex-start', alignItems: 'center', paddingTop: vertical ? '9%' : '6%' }}>
      <Screenshot src="shots/holdings.png" w={w} h={h} title="xlh · 持仓建议" pan />
      <Caption text="跨股选股 · 逐只持仓建议" sub="对每一只持仓给出加/持/减/止盈+建议金额，还提示集中度风险" />
    </AbsoluteFill>
  );
};
