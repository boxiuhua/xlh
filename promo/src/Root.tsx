import { Composition } from 'remotion';
import { Promo } from './Promo';
import { FPS, TOTAL } from './theme';

export const Root: React.FC = () => (
  <>
    <Composition id="PromoVertical" component={Promo} durationInFrames={TOTAL} fps={FPS} width={1080} height={1920} />
    <Composition id="PromoWide" component={Promo} durationInFrames={TOTAL} fps={FPS} width={1920} height={1080} />
  </>
);
