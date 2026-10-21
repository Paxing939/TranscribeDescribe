export type PromptData = {
  view: 'prompt'; app: string; windowTitle?: string; screens: number[];
  defaultMode: 'audio' | 'screen'; defaultScreen: number; deadlineMs: number;
  showCloudToggle: boolean; remindConsent: boolean;
};
export type KeepData = { view: 'keep'; app: string };
export type PillData = {
  view: 'pill'; app: string; startedMs: number; paused: boolean; pausedTotalMs: number;
  pauseStartedMs?: number; style: 'full' | 'dot';
};
export type EndData = { view: 'end'; reason: 'signal_lost' | 'silence' | 'max_duration'; deadlineMs: number };
export type NamingData = { view: 'naming'; deadlineMs: number; showCloudToggle?: boolean; cloudAllowed?: boolean; provider?: string };
export type DoneData = { view: 'done'; title: string | null; autoApplyMs: number | null; hasSummary: boolean };
export type ErrorData = {
  view: 'error'; title: string; message: string; info?: boolean; actions: { id: string; label: string }[];
};
export type ShortData = { view: 'short'; durationSec: number };

export type ViewData =
  | PromptData | KeepData | PillData | EndData | NamingData | DoneData | ErrorData | ShortData;
export type ViewName = ViewData['view'] | 'history' | 'settings';
