import { Img, staticFile, useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

// 真实微信二维码（promo/public/qr.png）。
export const QRPlaceholder: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const w = vertical ? width * 0.34 : width * 0.16;
  const fs = vertical ? width * 0.05 : width * 0.028;
  return (
    <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 14, fontFamily }}>
      <Img
        src={staticFile('qr.png')}
        style={{ width: w, height: w * 1.3, objectFit: 'contain', background: '#fff', borderRadius: 14 }}
      />
      <div style={{ color: COLORS.gold, fontSize: fs, fontWeight: 900 }}>微信 I1346535693</div>
    </div>
  );
};
