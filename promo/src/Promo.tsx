import { AbsoluteFill, Series } from 'remotion';
import { SCENES } from './theme';
import { Bg } from './components/Bg';
import { AudioTrack } from './components/AudioTrack';
import { S1Hook } from './scenes/S1Hook';
import { S2Logo } from './scenes/S2Logo';
import { S3Backtest } from './scenes/S3Backtest';
import { SOptimize } from './scenes/SOptimize';
import { S4Diagnose } from './scenes/S4Diagnose';
import { SStockTech } from './scenes/SStockTech';
import { SMarket } from './scenes/SMarket';
import { SPicks } from './scenes/SPicks';
import { S5Picks } from './scenes/S5Picks';
import { S6Push } from './scenes/S6Push';
import { S7History } from './scenes/S7History';
import { SStats } from './scenes/SStats';
import { S8Cta } from './scenes/S8Cta';

const MAP: Record<string, React.FC> = {
  S1Hook, S2Logo, S3Backtest, SOptimize, S4Diagnose, SStockTech, SMarket,
  SPicks, S5Picks, S6Push, S7History, SStats, S8Cta,
};

export const Promo: React.FC = () => (
  <AbsoluteFill>
    <Bg />
    <Series>
      {SCENES.map((s) => {
        const C = MAP[s.name];
        return (
          <Series.Sequence key={s.name} durationInFrames={s.durationInFrames}>
            <C />
          </Series.Sequence>
        );
      })}
    </Series>
    <AudioTrack />
  </AbsoluteFill>
);
