import { loadFont } from '@remotion/google-fonts/NotoSansSC';

export const { fontFamily } = loadFont();

export const FPS = 30;
export const TOTAL = 3600; // 120s @ 30fps

// 鲜艳张扬配色（暗紫底 + 霓虹强调色；金融惯例红涨绿跌用高饱和版）
export const COLORS = {
  bg0: '#0a0618',
  bg1: '#241056',
  up: '#ff3b5c',
  down: '#2ee6a6',
  accent: '#4f8cff',
  cyan: '#22d3ee',
  purple: '#a855f7',
  pink: '#ec4899',
  orange: '#fb923c',
  gold: '#ffd23f',
  text: '#ffffff',
  sub: '#c9bbff',
};

// 渐变色带（标题渐变、背景光晕用）
export const GRAD = {
  title: `linear-gradient(90deg, ${COLORS.cyan}, ${COLORS.accent}, ${COLORS.purple})`,
  warm: `linear-gradient(90deg, ${COLORS.gold}, ${COLORS.orange}, ${COLORS.pink})`,
};

export type SceneDef = { name: string; durationInFrames: number };

// 13 场景，合计 3600 帧（120s）
export const SCENES: SceneDef[] = [
  { name: 'S1Hook', durationInFrames: 300 },      // 痛点 10s
  { name: 'S2Logo', durationInFrames: 240 },      // 亮相 8s
  { name: 'S3Backtest', durationInFrames: 330 },  // 回测(真实界面) 11s
  { name: 'SOptimize', durationInFrames: 240 },   // 参数寻优 8s (NEW)
  { name: 'S4Diagnose', durationInFrames: 270 },  // 市场状态诊断 9s
  { name: 'SStockTech', durationInFrames: 300 },  // 股票技术诊断 10s (NEW)
  { name: 'SMarket', durationInFrames: 240 },     // 多市场行情 8s (NEW)
  { name: 'SPicks', durationInFrames: 300 },      // 选股推荐 10s (NEW)
  { name: 'S5Picks', durationInFrames: 360 },     // 持仓建议(真实界面) 12s
  { name: 'S6Push', durationInFrames: 300 },      // 自动推送 10s
  { name: 'S7History', durationInFrames: 240 },   // 历史可控 8s
  { name: 'SStats', durationInFrames: 240 },      // 数据总览 8s (NEW)
  { name: 'S8Cta', durationInFrames: 240 },       // CTA 8s
];
