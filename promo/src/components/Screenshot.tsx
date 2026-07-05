import { Img, interpolate, spring, staticFile, useCurrentFrame, useVideoConfig } from 'remotion';
import { COLORS } from '../theme';

// 把真实界面截图装进一个圆角「浏览器窗口」框，浮在暗色场景上作实拍插入。
// pan=true 时随帧缓慢下移，露出截图更下方的内容（如收益曲线）。
export const Screenshot: React.FC<{ src: string; w: number; h: number; title?: string; pan?: boolean }> = ({
  src,
  w,
  h,
  title = 'xlh 投资研判系统',
  pan,
}) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const s = spring({ frame, fps, config: { damping: 18 } });
  const scale = interpolate(s, [0, 1], [0.86, 1]);
  const opacity = interpolate(frame, [0, 12], [0, 1], { extrapolateRight: 'clamp' });
  const bar = Math.max(26, h * 0.055);
  const dot = bar * 0.26;
  const panY = pan ? interpolate(frame, [24, 280], [0, -16], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }) : 0;
  return (
    <div
      style={{
        width: w,
        transform: `scale(${scale})`,
        opacity,
        borderRadius: 18,
        overflow: 'hidden',
        boxShadow: '0 30px 90px rgba(0,0,0,0.55)',
        border: '1px solid rgba(255,255,255,0.14)',
      }}
    >
      <div style={{ height: bar, background: '#1b2233', display: 'flex', alignItems: 'center', gap: dot * 0.7, padding: `0 ${bar * 0.5}px` }}>
        <span style={{ width: dot, height: dot, borderRadius: '50%', background: '#ff5f57' }} />
        <span style={{ width: dot, height: dot, borderRadius: '50%', background: '#febc2e' }} />
        <span style={{ width: dot, height: dot, borderRadius: '50%', background: '#28c840' }} />
        <span style={{ marginLeft: bar * 0.4, color: COLORS.sub, fontSize: bar * 0.42 }}>{title}</span>
      </div>
      <div style={{ height: h, overflow: 'hidden', background: '#fff' }}>
        <Img src={staticFile(src)} style={{ width: '100%', display: 'block', transform: `translateY(${panY}%)` }} />
      </div>
    </div>
  );
};
