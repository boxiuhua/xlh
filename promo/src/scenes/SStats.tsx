import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { COLORS, fontFamily, GRAD } from '../theme';

const STATS = [
  { to: 3, dec: 0, suffix: ' 大', unit: '市场', desc: 'A股 / 港股 / 美股' },
  { to: 5, dec: 0, suffix: '+', unit: '内置策略', desc: '定投 / 择时 / RSI / 自适应' },
  { to: 18.4, dec: 1, suffix: '%', unit: '回测年化', desc: '示例 000834 智能定投' },
  { to: 4, dec: 0, suffix: ' 渠道', unit: '自动推送', desc: '钉钉 / 飞书 / 企微 / 微信' },
];

export const SStats: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const numFs = vertical ? width * 0.11 : width * 0.06;
  const unitFs = vertical ? width * 0.04 : width * 0.022;
  return (
    <AbsoluteFill style={{ justifyContent: 'center', alignItems: 'center', paddingBottom: vertical ? '14%' : '8%' }}>
      <div
        style={{
          display: 'grid',
          gridTemplateColumns: vertical ? '1fr 1fr' : '1fr 1fr 1fr 1fr',
          gap: vertical ? '36px 28px' : 48,
          maxWidth: '90%',
        }}
      >
        {STATS.map((st, i) => {
          const start = 10 + i * 12;
          const p = interpolate(frame, [start, start + 34], [0, 1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' });
          const val = (st.to * p).toFixed(st.dec);
          const s = spring({ frame: frame - start, fps, config: { damping: 14 } });
          return (
            <div key={st.unit} style={{ textAlign: 'center', fontFamily, opacity: Math.max(0, s), transform: `scale(${interpolate(s, [0, 1], [0.7, 1])})` }}>
              <div
                style={{
                  fontSize: numFs,
                  fontWeight: 900,
                  lineHeight: 1,
                  backgroundImage: GRAD.warm,
                  WebkitBackgroundClip: 'text',
                  backgroundClip: 'text',
                  WebkitTextFillColor: 'transparent',
                  filter: 'drop-shadow(0 0 20px rgba(255,146,60,0.5))',
                }}
              >
                {val}
                <span style={{ fontSize: numFs * 0.5 }}>{st.suffix}</span>
              </div>
              <div style={{ color: COLORS.text, fontSize: unitFs, fontWeight: 800, marginTop: 8 }}>{st.unit}</div>
              <div style={{ color: COLORS.sub, fontSize: unitFs * 0.72, marginTop: 4 }}>{st.desc}</div>
            </div>
          );
        })}
      </div>
      <Caption text="一套系统，全流程搞定" sub="从数据、回测、诊断到持仓建议与自动推送，一站到底" />
    </AbsoluteFill>
  );
};
