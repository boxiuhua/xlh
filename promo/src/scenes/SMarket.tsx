import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { MarketTags } from '../components/MarketTags';
import { COLORS, fontFamily } from '../theme';

const TICKERS = [
  { name: '贵州茅台', code: 'A股 600519', chg: '+2.14%', up: true },
  { name: '腾讯控股', code: '港股 00700', chg: '+1.03%', up: true },
  { name: '苹果 AAPL', code: '美股 AAPL', chg: '-0.86%', up: false },
  { name: '宁德时代', code: 'A股 300750', chg: '+3.27%', up: true },
];

export const SMarket: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const fs = vertical ? width * 0.04 : width * 0.022;
  return (
    <AbsoluteFill
      style={{
        flexDirection: 'column',
        justifyContent: 'center',
        alignItems: 'center',
        gap: vertical ? 34 : 28,
        paddingBottom: vertical ? '13%' : '7%',
      }}
    >
      <MarketTags />
      <div style={{ display: 'flex', flexDirection: 'column', gap: vertical ? 12 : 10, width: vertical ? '82%' : '46%' }}>
        {TICKERS.map((tk, i) => {
          const s = spring({ frame: frame - (60 + i * 14), fps, config: { damping: 16 } });
          const c = tk.up ? COLORS.up : COLORS.down;
          return (
            <div
              key={tk.code}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 14,
                padding: '12px 20px',
                borderRadius: 12,
                background: 'rgba(255,255,255,0.05)',
                fontFamily,
                opacity: Math.max(0, s),
                transform: `translateX(${interpolate(s, [0, 1], [40, 0])}px)`,
              }}
            >
              <div style={{ fontSize: fs, fontWeight: 800, color: COLORS.text, flex: 1 }}>{tk.name}</div>
              <div style={{ fontSize: fs * 0.72, color: COLORS.sub }}>{tk.code}</div>
              <div style={{ fontSize: fs, fontWeight: 900, color: c, width: fs * 4, textAlign: 'right' }}>{tk.chg}</div>
            </div>
          );
        })}
      </div>
      <Caption text="A股 · 港股 · 美股 全覆盖" sub="一个系统盯三大市场，行情、诊断、回测通吃" />
    </AbsoluteFill>
  );
};
