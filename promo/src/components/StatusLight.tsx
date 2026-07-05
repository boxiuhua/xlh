import { interpolate, useCurrentFrame } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const STATES = [
  { label: '上涨趋势', color: COLORS.up },
  { label: '震荡', color: COLORS.sub },
  { label: '下跌趋势', color: COLORS.down },
];

export const StatusLight: React.FC<{ size: number }> = ({ size }) => {
  const frame = useCurrentFrame();
  const active = Math.min(STATES.length - 1, Math.floor(frame / 45) % STATES.length);
  return (
    <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 20 }}>
      <div style={{ display: 'flex', gap: 28 }}>
        {STATES.map((s, i) => {
          const on = i === active ? 1 : 0.2;
          const pulse = i === active ? interpolate(frame % 45, [0, 22, 45], [0.6, 1, 0.6]) : 0.2;
          return (
            <div
              key={s.label}
              style={{
                width: size,
                height: size,
                borderRadius: '50%',
                background: s.color,
                opacity: on,
                boxShadow: i === active ? `0 0 ${size * 0.8}px ${s.color}` : 'none',
                transform: `scale(${0.9 + pulse * 0.2})`,
              }}
            />
          );
        })}
      </div>
      <div style={{ fontFamily, fontSize: size * 0.9, fontWeight: 800, color: STATES[active].color }}>
        {STATES[active].label}
      </div>
    </div>
  );
};
