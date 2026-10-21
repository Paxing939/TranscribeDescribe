<script lang="ts">
  import type { PillData } from '../lib/types';
  import { action } from '../lib/api';
  import { clock, hms } from '../lib/clock.svelte';
  let { data }: { data: PillData } = $props();
  let hover = $state(false);
  let flash = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;

  const elapsed = $derived(
    clock.now - data.startedMs - data.pausedTotalMs -
      (data.paused ? clock.now - (data.pauseStartedMs ?? clock.now) : 0)
  );
  const full = $derived(data.style === 'full' || hover);

  function bookmark() {
    action('bookmark');
    flash = true;
    clearTimeout(timer);
    timer = setTimeout(() => (flash = false), 1500);
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="pill" class:compact={!full} data-tauri-drag-region
     onmouseenter={() => (hover = true)} onmouseleave={() => (hover = false)}>
  <span class="dot" class:paused={data.paused} data-tauri-drag-region></span>
  {#if full}
    <span class="time" data-tauri-drag-region>{hms(elapsed)}</span>
    <span class="app" data-tauri-drag-region>{flash ? 'Метка поставлена' : data.app}</span>
    <span class="spacer" data-tauri-drag-region></span>
    {#if data.paused}
      <button onclick={() => action('resume')}>▶ Продолжить</button>
    {:else}
      <button onclick={() => action('pause')}>⏸ Пауза</button>
    {/if}
    <button onclick={bookmark}>🔖 Метка</button>
    <button class="danger" onclick={() => action('stop')}>⏹ Стоп</button>
  {/if}
</div>

<style>
  .pill {
    width: 100%; height: 100%; display: flex; align-items: center; gap: 8px;
    padding: 0 10px; background: var(--bg); border: 1px solid var(--border);
    border-radius: 12px; overflow: hidden; white-space: nowrap;
  }
  .pill.compact { justify-content: center; padding: 0; }
  .dot { width: 10px; height: 10px; border-radius: 50%; background: #e11d48; flex: none; animation: pulse 1.4s ease-in-out infinite; }
  .dot.paused { background: var(--muted); animation: none; }
  .compact .dot { width: 12px; height: 12px; }
  .time { font-variant-numeric: tabular-nums; font-weight: 600; }
  .app { color: var(--muted); overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  button { padding: 3px 7px; flex: none; }
  @keyframes pulse { 0%,100% { opacity: 1; transform: scale(1); } 50% { opacity: .35; transform: scale(.8); } }
</style>
