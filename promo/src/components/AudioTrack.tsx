import { Audio, Sequence, staticFile } from 'remotion';

// 各场景 AI 配音（edge-tts 生成，promo/public/vo/*.mp3），按场景起始帧对齐。
// 想换音色/文案：重跑 edge-tts 覆盖对应 mp3（见 README）。想加 BGM：在下方 <Audio src={staticFile('bgm.mp3')} volume={0.12}/> 并放入 public/bgm.mp3。
const VO: { file: string; from: number }[] = [
  { file: 'vo/s1.mp3', from: 0 },
  { file: 'vo/s2.mp3', from: 210 },
  { file: 'vo/s3.mp3', from: 420 },
  { file: 'vo/s4.mp3', from: 720 },
  { file: 'vo/s5.mp3', from: 990 },
  { file: 'vo/s6.mp3', from: 1290 },
  { file: 'vo/s7.mp3', from: 1530 },
  { file: 'vo/s8.mp3', from: 1710 },
];

export const AudioTrack: React.FC = () => (
  <>
    {VO.map((v) => (
      <Sequence key={v.file} from={v.from}>
        <Audio src={staticFile(v.file)} />
      </Sequence>
    ))}
  </>
);
