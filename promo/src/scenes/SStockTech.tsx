import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { GrowLine } from '../components/GrowLine';
import { COLORS, fontFamily } from '../theme';

const IND = [
  { k: 'MA', v: '多头排列', c: COLORS.up },
  { k: 'MACD', v: '金叉', c: COLORS.up },
  { k: 'RSI', v: '58 中性', c: COLORS.gold },
  { k: '布林', v: '中轨上方', c: COLORS.cyan },
];

export const SStockTech: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const fs = vertical ? width * 0.04 : width * 0.022;
  const sig = spring({ frame: frame - 90, fps, config: { damping: 12, stiffness: 120 } });
  const lineW = vertical ? width * 0.84 : width * 0.5;
  const lineH = vertical ? height * 0.2 : height * 0.34;
  return (
    <AbsoluteFill
      style={{
        flexDirection: 'column',
        justifyContent: 'center',
        alignItems: 'center',
        gap: vertical ? 34 : 30,
        paddingTop: vertical ? '6%' : 0,
        paddingBottom: vertical ? '14%' : '8%',
      }}
    >
      <GrowLine w={lineW} h={lineH} />
      <div style={{ display: 'flex', gap: vertical ? 14 : 22, flexWrap: 'wrap', justifyContent: 'center', maxWidth: '92%' }}>
        {IND.map((it, i) => {
          const s = spring({ frame: frame - (20 + i * 10), fps, config: { damping: 14 } });
          return (
            <div
              key={it.k}
              style={{
                opacity: Math.max(0, s),
                transform: `translateY(${interpolate(s, [0, 1], [24, 0])}px)`,
                fontFamily,
                padding: '10px 20px',
                borderRadius: 14,
                background: 'rgba(255,255,255,0.06)',
                border: `1px solid ${it.c}`,
                boxShadow: `0 0 18px ${it.c}55`,
                textAlign: 'center',
              }}
            >
              <div style={{ color: COLORS.sub, fontSize: fs * 0.66, fontWeight: 700 }}>{it.k}</div>
              <div style={{ color: it.c, fontSize: fs, fontWeight: 900, marginTop: 4 }}>{it.v}</div>
            </div>
          );
        })}
      </div>
      <div
        style={{
          fontFamily,
          fontSize: fs * 1.35,
          fontWeight: 900,
          color: '#fff',
          background: COLORS.up,
          padding: '8px 34px',
          borderRadius: 999,
          transform: `scale(${interpolate(sig, [0, 1], [0.5, 1])})`,
          opacity: Math.max(0, sig),
          boxShadow: `0 0 36px ${COLORS.up}`,
        }}
      >
        综合信号：买入
      </div>
      <Caption text="股票技术诊断" sub="MA / MACD / 布林 / RSI 综合打分，直接给出买入·观望·卖出信号" />
    </AbsoluteFill>
  );
};
