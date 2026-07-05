import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { StockCard } from '../components/StockCard';

const PICKS = [
  { rank: 1, name: '贵州茅台', score: 9.2 },
  { rank: 2, name: '宁德时代', score: 8.7 },
  { rank: 3, name: '比亚迪', score: 8.4 },
  { rank: 4, name: '腾讯控股', score: 8.1 },
];

export const SPicks: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  return (
    <AbsoluteFill
      style={{
        flexDirection: 'column',
        justifyContent: 'center',
        alignItems: 'center',
        gap: vertical ? 16 : 14,
        paddingBottom: vertical ? '13%' : '7%',
      }}
    >
      {PICKS.map((p, i) => (
        <StockCard key={p.name} {...p} delay={20 + i * 14} />
      ))}
      <Caption text="智能选股 · 跨市场评分排名" sub="全市场扫描、样本外评分，把最值得关注的标的排在最前面" />
    </AbsoluteFill>
  );
};
