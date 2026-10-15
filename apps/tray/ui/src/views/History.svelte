<script lang="ts">
  // F-90: history of calls with full-text search and actions.
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { inTauri } from '../lib/api';

  type Row = {
    id: string; callDir: string; status: string; title: string | null; app: string;
    startedAt: string; durationSec: number | null; hasVideo: boolean; hasSummary: boolean;
  };

  let rows = $state<Row[]>([]);
  let query = $state('');
  let busy = $state(false);
  let message = $state('');
  let renaming = $state<string | null>(null);
  let newTitle = $state('');

  const STATUS: Record<string, string> = {
    done: 'готово', awaiting_name: 'ждёт названия', queued: 'в очереди', transcribing: 'расшифровка',
    summarizing: 'саммари', summary_pending: 'саммари позже', failed: 'ошибка', recorded: 'только запись',
    exported: 'на другой машине', recording: 'запись',
  };

  async function load() {
    if (!inTauri) {
      rows = [{ id: '1', callDir: 'C:/x', status: 'done', title: 'Обсуждение CI', app: 'Zoom',
        startedAt: '2026-10-15T10:00:00+03:00', durationSec: 2712, hasVideo: true, hasSummary: true }];
      return;
    }
    busy = true;
    try { rows = await invoke<Row[]>('history_list', { query: query.trim() }); message = ''; }
    catch (e) { message = String(e); }
    finally { busy = false; }
  }

  async function act(id: string, action: string, value?: string) {
    if (action === 'delete' && !confirm('Удалить звонок вместе с записью и транскриптом?')) return;
    try {
      await invoke('history_action', { id, action, value: value ?? null });
      if (['rename', 'delete', 'retranscribe', 'resummarize'].includes(action)) await load();
      if (action === 'retranscribe' || action === 'resummarize') message = 'Поставлено в очередь';
    } catch (e) { message = String(e); }
  }

  function fmtDate(s: string) {
    const d = new Date(s);
    return isNaN(+d) ? s : d.toLocaleString('ru-RU', { day: '2-digit', month: '2-digit', year: 'numeric', hour: '2-digit', minute: '2-digit' });
  }
  function fmtDur(s: number | null) {
    if (s == null) return '—';
    const m = Math.round(s / 60);
    return m >= 60 ? `${Math.floor(m / 60)} ч ${m % 60} мин` : `${m} мин`;
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  function onSearch() { clearTimeout(timer); timer = setTimeout(load, 250); }

  onMount(load);
</script>

<div class="page">
  <div class="top">
    <input class="search" placeholder="Поиск по названиям, транскриптам и саммари…" bind:value={query} oninput={onSearch} aria-label="Поиск" />
    <button onclick={load} disabled={busy}>Обновить</button>
  </div>
  {#if message}<div class="msg">{message}</div>{/if}
  <div class="table">
    <table>
      <thead><tr><th>Дата</th><th>Длит.</th><th>Приложение</th><th>Название</th><th>Статус</th><th></th></tr></thead>
      <tbody>
        {#each rows as r (r.id)}
          <tr>
            <td class="nowrap">{fmtDate(r.startedAt)}</td>
            <td class="nowrap">{fmtDur(r.durationSec)}</td>
            <td>{r.app}</td>
            <td class="title-cell">
              {#if renaming === r.id}
                <input bind:value={newTitle} aria-label="Новое название"
                  onkeydown={(e) => { if (e.key === 'Enter') { act(r.id, 'rename', newTitle); renaming = null; } if (e.key === 'Escape') renaming = null; }} />
              {:else}{r.title ?? '—'}{/if}
            </td>
            <td class="nowrap" class:bad={r.status === 'failed'}>{STATUS[r.status] ?? r.status}</td>
            <td class="actions">
              {#if r.hasVideo}<button onclick={() => act(r.id, 'open_video')} title="Открыть запись">▶</button>{/if}
              {#if r.hasSummary}<button onclick={() => act(r.id, 'open_summary')} title="Открыть саммари">📝</button>{/if}
              <button onclick={() => act(r.id, 'open_folder')} title="Открыть папку">📁</button>
              <button onclick={() => { renaming = r.id; newTitle = r.title ?? ''; }} title="Переименовать">✎</button>
              <button onclick={() => act(r.id, 'retranscribe')} title="Расшифровать заново">⟳</button>
              <button onclick={() => act(r.id, 'resummarize')} title="Пересоздать саммари">Σ</button>
              <button class="danger" onclick={() => act(r.id, 'delete')} title="Удалить">✕</button>
            </td>
          </tr>
        {:else}
          <tr><td colspan="6" class="empty">{busy ? 'Загрузка…' : 'Звонков нет'}</td></tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>

<style>
  .page { display: flex; flex-direction: column; height: 100%; padding: 12px; gap: 8px; user-select: text; -webkit-user-select: text; }
  .top { display: flex; gap: 8px; }
  .search { flex: 1; padding: 6px 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--input-bg); color: var(--fg); font: inherit; }
  .msg { color: var(--muted); font-size: 12px; }
  .table { flex: 1; overflow: auto; border: 1px solid var(--border); border-radius: 8px; }
  table { width: 100%; border-collapse: collapse; }
  th { position: sticky; top: 0; background: var(--btn-bg); text-align: left; font-weight: 600; font-size: 12px; padding: 6px 8px; }
  td { padding: 5px 8px; border-top: 1px solid var(--border); vertical-align: middle; }
  .nowrap { white-space: nowrap; }
  .title-cell { width: 40%; }
  .title-cell input { width: 100%; font: inherit; padding: 2px 6px; background: var(--input-bg); color: var(--fg); border: 1px solid var(--accent); border-radius: 5px; }
  .actions { white-space: nowrap; text-align: right; }
  .actions button { padding: 2px 6px; margin-left: 2px; }
  .bad { color: var(--danger); }
  .empty { text-align: center; color: var(--muted); padding: 24px; }
  :global(html:has(.page)), :global(body:has(.page)) { overflow: hidden; }
</style>
