import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const actionColor = (a: string) =>
  a === '加仓' || a === '止盈' ? COLORS.up : a === '减仓' ? COLORS.down : COLORS.sub;

export const HoldingRow: React.FC<{ name: string; action: string; amount: string; delay: number }> = ({
  name,
  action,
  amount,
  delay,
}) => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const vertical = height >= width;
  const s = spring({ frame: frame - delay, fps, config: { damping: 16 } });
  const fs = vertical ? width * 0.042 : width * 0.022;
  const tag = interpolate(s, [0, 1], [0.6, 1]);
  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 16,
        padding: '12px 20px',
        borderRadius: 12,
        background: 'rgba(255,255,255,0.04)',
        fontFamily,
        opacity: Math.max(0, s),
        width: vertical ? '80%' : 460,
      }}
    >
      <div style={{ fontSize: fs, fontWeight: 700, color: COLORS.text, flex: 1 }}>{name}</div>
      <div
        style={{
          fontSize: fs * 0.85,
          fontWeight: 800,
          color: '#fff',
          background: actionColor(action),
          padding: '4px 14px',
          borderRadius: 999,
          transform: `scale(${tag})`,
        }}
      >
        {action}
      </div>
      <div style={{ fontSize: fs * 0.8, color: COLORS.sub, width: fs * 4, textAlign: 'right' }}>{amount}</div>
    </div>
  );
};
