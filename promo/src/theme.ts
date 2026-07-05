import { loadFont } from '@remotion/google-fonts/NotoSansSC';

export const { fontFamily } = loadFont();

export const FPS = 30;
export const TOTAL = 1800;

export const COLORS = {
  bg0: '#0b1020',
  bg1: '#111827',
  up: '#c0392b',
  down: '#27ae60',
  accent: '#3b82f6',
  text: '#e5e7eb',
  sub: '#94a3b8',
  gold: '#facc15',
};

export type SceneDef = { name: string; durationInFrames: number };

export const SCENES: SceneDef[] = [
  { name: 'S1Hook', durationInFrames: 210 },
  { name: 'S2Logo', durationInFrames: 210 },
  { name: 'S3Backtest', durationInFrames: 300 },
  { name: 'S4Diagnose', durationInFrames: 270 },
  { name: 'S5Picks', durationInFrames: 300 },
  { name: 'S6Push', durationInFrames: 240 },
  { name: 'S7History', durationInFrames: 180 },
  { name: 'S8Cta', durationInFrames: 90 },
];
