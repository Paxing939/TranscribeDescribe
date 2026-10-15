<script lang="ts">
  import type { DoneData } from '../lib/types';
  import { action } from '../lib/api';
  import { secsLeft } from '../lib/clock.svelte';
  let { data }: { data: DoneData } = $props();
  let editing = $state(false);
  let draft = $state('');
  const mins = $derived(data.autoApplyMs ? Math.max(1, Math.ceil(secsLeft(data.autoApplyMs) / 60)) : 0);
  const save = () => action('edit', { title: draft.trim() });
  function startEdit() { draft = data.title ?? ''; editing = true; }
  function focusEl(el: HTMLInputElement) { el.focus(); el.select(); }
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape') { if (editing) editing = false; else action('dismiss'); }
    else if (e.key === 'Enter' && editing && (e.target as HTMLElement)?.tagName !== 'BUTTON') save();
  }
</script>

<svelte:window onkeydown={onkey} />
<div class="card">
  <button class="close" aria-label="Закрыть" onclick={() => action('dismiss')}>×</button>
  <p class="title">{data.title ? `Готово: „${data.title}“` : 'Готово'}</p>
  {#if editing}
    <label class="sub" for="edit">Название</label>
    <input id="edit" type="text" bind:value={draft} use:focusEl autocomplete="off" />
    <div class="row">
      <button class="primary" onclick={save}>Сохранить</button>
      <button onclick={() => (editing = false)}>Отмена</button>
    </div>
  {:else}
    <div class="row">
      <button class="primary" onclick={() => action('apply')}>Применить</button>
      <button onclick={startEdit}>Изменить</button>
      {#if data.hasSummary}<button onclick={() => action('open_summary')}>Открыть саммари</button>{/if}
      <button onclick={() => action('open_folder')}>Открыть папку</button>
    </div>
    {#if data.autoApplyMs}<div class="sub">применится автоматически через {mins} мин</div>{/if}
  {/if}
</div>
