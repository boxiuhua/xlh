import { interpolate, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

const DATES = ['06-28', '07-01', '07-03', '07-05'];

export const Timeline: React.FC = () => {
  const frame = useCurrentFrame();
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const fs = vertical ? width * 0.04 : width * 0.022;
  const slide = interpolate(frame, [0, 60], [40, 0], { extrapolateRight: 'clamp' });
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: vertical ? 18 : 30, transform: `translateX(${slide}px)`, fontFamily }}>
      {DATES.map((d, i) => {
        const on = interpolate(frame, [i * 16, i * 16 + 16], [0, 1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' });
        return (
          <div key={d} style={{ display: 'flex', alignItems: 'center', gap: vertical ? 18 : 30 }}>
            <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 8, opacity: 0.4 + on * 0.6 }}>
              <div style={{ width: fs * 0.7, height: fs * 0.7, borderRadius: '50%', background: COLORS.accent }} />
              <div style={{ color: COLORS.sub, fontSize: fs * 0.7 }}>{d}</div>
            </div>
            {i < DATES.length - 1 ? <div style={{ width: vertical ? 30 : 60, height: 3, background: 'rgba(255,255,255,0.2)' }} /> : null}
          </div>
        );
      })}
    </div>
  );
};
