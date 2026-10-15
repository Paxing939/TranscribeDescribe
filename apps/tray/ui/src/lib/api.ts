import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { ViewData, ViewName } from './types';
import { mockData } from './mock';

export const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

export function currentView(): ViewName {
  return (location.hash.replace(/^#/, '') || 'prompt') as ViewName;
}

export async function loadData(): Promise<ViewData> {
  if (!inTauri) return mockData(currentView());
  return (await invoke('view_data')) as ViewData;
}

export async function onUpdate(cb: (d: ViewData) => void): Promise<() => void> {
  if (!inTauri) return () => {};
  return await listen<ViewData>('view-update', (e) => cb(e.payload));
}

export async function action(name: string, value?: unknown): Promise<void> {
  if (!inTauri) {
    console.log('[mock ui_action]', name, value);
    return;
  }
  try {
    await invoke('ui_action', value === undefined ? { action: name } : { action: name, value });
  } catch (e) {
    console.error('ui_action failed', e);
  }
}
