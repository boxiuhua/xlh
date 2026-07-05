import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const MSGS = [
  { app: '微信', text: '持仓建议已更新：沪深300 建议加仓' },
  { app: '钉钉', text: '基金诊断：当前震荡，注意仓位' },
  { app: '飞书', text: '每日推送 · 3 只持仓 2 加 1 止盈' },
];

export const PhoneNotify: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const phoneW = vertical ? width * 0.6 : height * 0.62;
  const phoneH = phoneW * 2.05;
  const fs = phoneW * 0.062;
  return (
    <div
      style={{
        width: phoneW,
        height: phoneH,
        borderRadius: phoneW * 0.12,
        border: `${phoneW * 0.02}px solid #333`,
        background: '#05070f',
        padding: phoneW * 0.06,
        display: 'flex',
        flexDirection: 'column',
        gap: phoneW * 0.045,
        boxShadow: '0 20px 80px rgba(0,0,0,0.6)',
      }}
    >
      {MSGS.map((m, i) => {
        const s = spring({ frame: frame - (20 + i * 34), fps, config: { damping: 16 } });
        const y = interpolate(s, [0, 1], [-40, 0]);
        return (
          <div
            key={m.app}
            style={{
              background: 'rgba(255,255,255,0.06)',
              borderRadius: phoneW * 0.05,
              padding: phoneW * 0.05,
              fontFamily,
              opacity: Math.max(0, s),
              transform: `translateY(${y}px)`,
            }}
          >
            <div style={{ color: COLORS.accent, fontSize: fs * 0.8, fontWeight: 800 }}>{m.app}</div>
            <div style={{ color: COLORS.text, fontSize: fs, fontWeight: 600, marginTop: 6, lineHeight: 1.3 }}>{m.text}</div>
          </div>
        );
      })}
    </div>
  );
};
