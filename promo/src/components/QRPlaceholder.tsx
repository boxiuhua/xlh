import { useVideoConfig } from 'remotion';
import { COLORS, fontFamily } from '../theme';

// 二维码占位：把真实二维码放 promo/public/qr.png 后，可改为 <Img src={staticFile('qr.png')} />。
export const QRPlaceholder: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  const box = vertical ? width * 0.28 : width * 0.14;
  const fs = vertical ? width * 0.05 : width * 0.028;
  return (
    <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 14, fontFamily }}>
      <div
        style={{
          width: box,
          height: box,
          background: '#fff',
          borderRadius: 12,
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          color: '#111',
          fontSize: fs * 0.5,
          fontWeight: 700,
        }}
      >
        二维码
      </div>
      <div style={{ color: COLORS.gold, fontSize: fs, fontWeight: 900 }}>微信 I1346535693</div>
    </div>
  );
};
