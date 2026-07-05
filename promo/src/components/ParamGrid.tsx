import { interpolate, useCurrentFrame } from 'remotion';
import { COLORS } from '../theme';

// 参数网格逐格点亮，最后锁定"最优"格（右下角）。
export const ParamGrid: React.FC<{ size: number }> = ({ size }) => {
  const frame = useCurrentFrame();
  const cols = 5;
  const rows = 5;
  const cell = size / cols;
  const best = { r: 3, c: 4 };
  return (
    <svg width={size} height={cell * rows}>
      {Array.from({ length: rows }).map((_, r) =>
        Array.from({ length: cols }).map((__, c) => {
          const idx = r * cols + c;
          const on = interpolate(frame, [60 + idx * 4, 72 + idx * 4], [0, 1], {
            extrapolateLeft: 'clamp',
            extrapolateRight: 'clamp',
          });
          const isBest = r === best.r && c === best.c;
          const lock = isBest ? interpolate(frame, [200, 230], [0, 1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }) : 0;
          const heat = 0.15 + on * 0.6;
          return (
            <rect
              key={`${r}-${c}`}
              x={c * cell + 4}
              y={r * cell + 4}
              width={cell - 8}
              height={cell - 8}
              rx={8}
              fill={isBest ? COLORS.gold : COLORS.accent}
              opacity={isBest ? 0.3 + lock * 0.7 : heat}
              stroke={isBest && lock > 0.2 ? COLORS.gold : 'transparent'}
              strokeWidth={4}
            />
          );
        })
      )}
    </svg>
  );
};
