import type { ViewData, ViewName } from './types';

export function mockData(view: ViewName): ViewData {
  const now = Date.now();
  switch (view) {
    case 'prompt':
      return { view, app: 'Zoom', windowTitle: 'Еженедельная планёрка — Zoom Meeting', screens: [1, 2],
        defaultMode: 'audio', defaultScreen: 1, deadlineMs: now + 20000, showCloudToggle: true, remindConsent: true };
    case 'keep': return { view, app: 'Teams' };
    case 'pill':
      return { view, app: 'Zoom', startedMs: now - 3725000, paused: false, pausedTotalMs: 0,
        style: new URLSearchParams(location.search).get('style') === 'dot' ? 'dot' : 'full' };
    case 'end': return { view, reason: 'silence', deadlineMs: now + 30000 };
    case 'naming': return { view, deadlineMs: now + 45000 };
    case 'done': return { view, title: 'Планёрка по релизу', autoApplyMs: now + 5 * 60000, hasSummary: true };
    case 'error':
      return { view, title: 'Ошибка транскрибации', message: 'Не удалось запустить воркер: нет доступа к GPU.',
        actions: [{ id: 'retry', label: 'Повторить' }, { id: 'open_logs', label: 'Открыть логи' }] };
    case 'short': return { view, durationSec: 42 };
    default: return { view: 'keep', app: '?' };
  }
}
