import { Audio, Sequence, staticFile } from 'remotion';

// 各场景 AI 配音（edge-tts 生成，promo/public/vo/*.mp3），按场景起始帧对齐。
// 想换音色/文案：重跑 edge-tts 覆盖对应 mp3（见 README）。想加 BGM：在下方 <Audio src={staticFile('bgm.mp3')} volume={0.12}/> 并放入 public/bgm.mp3。
// 13 段，对齐 13 个场景的累计起始帧（见 theme.ts SCENES）。
const VO: { file: string; from: number }[] = [
  { file: 'vo/s1.mp3', from: 0 },
  { file: 'vo/s2.mp3', from: 300 },
  { file: 'vo/s3.mp3', from: 540 },
  { file: 'vo/s4.mp3', from: 870 },
  { file: 'vo/s5.mp3', from: 1110 },
  { file: 'vo/s6.mp3', from: 1380 },
  { file: 'vo/s7.mp3', from: 1680 },
  { file: 'vo/s8.mp3', from: 1920 },
  { file: 'vo/s9.mp3', from: 2220 },
  { file: 'vo/s10.mp3', from: 2580 },
  { file: 'vo/s11.mp3', from: 2880 },
  { file: 'vo/s12.mp3', from: 3120 },
  { file: 'vo/s13.mp3', from: 3360 },
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
