import { AbsoluteFill } from 'remotion';
import { COLORS } from '../theme';

export const Bg: React.FC = () => (
  <AbsoluteFill
    style={{
      background: `radial-gradient(1200px 1200px at 50% 30%, ${COLORS.bg1}, ${COLORS.bg0})`,
    }}
  >
    <AbsoluteFill
      style={{
        backgroundImage:
          'linear-gradient(rgba(255,255,255,0.04) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.04) 1px, transparent 1px)',
        backgroundSize: '64px 64px',
        opacity: 0.6,
      }}
    />
  </AbsoluteFill>
);
