// 音轨槽：默认关闭（缺音频文件会导致渲染失败）。
// 启用：把 narration.mp3 / bgm.mp3 放入 promo/public/，取消下面注释。
//
// import { Audio, staticFile } from 'remotion';
// export const AudioTrack: React.FC = () => (
//   <>
//     <Audio src={staticFile('narration.mp3')} />
//     <Audio src={staticFile('bgm.mp3')} volume={0.15} />
//   </>
// );

export const AudioTrack: React.FC = () => null;
