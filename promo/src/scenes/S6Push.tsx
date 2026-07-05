import { AbsoluteFill, useVideoConfig } from 'remotion';
import { Caption } from '../components/Caption';
import { PhoneNotify } from '../components/PhoneNotify';

export const S6Push: React.FC = () => {
  const { width, height } = useVideoConfig();
  const vertical = height >= width;
  return (
    <AbsoluteFill style={{ justifyContent: 'center', alignItems: 'center', paddingTop: vertical ? '4%' : 0, paddingBottom: vertical ? '18%' : '8%' }}>
      <PhoneNotify />
      <Caption text="定时推送到微信 / 钉钉 / 飞书" sub="不用盯盘，建议自动送达" />
    </AbsoluteFill>
  );
};
