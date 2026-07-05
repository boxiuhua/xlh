import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { StockCard } from '../components/StockCard';
import { HoldingRow } from '../components/HoldingRow';

const PICKS = [
  { rank: 1, name: '贵州茅台', score: 9.2 },
  { rank: 2, name: '宁德时代', score: 8.7 },
  { rank: 3, name: '腾讯控股', score: 8.1 },
];
const HOLDINGS = [
  { name: '沪深300ETF', action: '加仓', amount: '+2,000' },
  { name: '中概互联', action: '持有', amount: '—' },
  { name: '白酒LOF', action: '止盈', amount: '-1,500' },
];

export const S5Picks: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  return (
    <AbsoluteFill
      style={{
        flexDirection: vertical ? 'column' : 'row',
        justifyContent: 'center',
        alignItems: 'center',
        gap: vertical ? 18 : 60,
        paddingTop: vertical ? '8%' : 0,
        paddingBottom: vertical ? '32%' : 0,
      }}
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: 14, alignItems: 'center' }}>
        {PICKS.map((p, i) => (
          <StockCard key={p.name} {...p} delay={20 + i * 14} />
        ))}
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 12, alignItems: 'center' }}>
        {HOLDINGS.map((h, i) => (
          <HoldingRow key={h.name} {...h} delay={110 + i * 16} />
        ))}
      </div>
      <Caption text="跨股选股 · 逐只持仓建议" sub="加仓 / 持有 / 减仓 / 止盈 + 建议金额" />
    </AbsoluteFill>
  );
};
