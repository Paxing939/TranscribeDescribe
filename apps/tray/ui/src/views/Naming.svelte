<script lang="ts">
  import type { NamingData } from '../lib/types';
  import { action } from '../lib/api';
  import { secsLeft } from '../lib/clock.svelte';
  import Countdown from '../lib/Countdown.svelte';
  let { data }: { data: NamingData } = $props();
  let title = $state('');
  // Sent on every change, so it also applies on «Пропустить» and when the dialog times out.
  let noCloud = $state(false);
  $effect(() => { noCloud = !(data.cloudAllowed ?? true); });
  const left = $derived(secsLeft(data.deadlineMs));
  const submit = () => action('submit', { title: title.trim() });
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape') action('skip');
    else if (e.key === 'Enter' && (e.target as HTMLElement)?.tagName === 'INPUT' && (e.target as HTMLInputElement).type === 'text') submit();
  }
  function focusEl(el: HTMLInputElement) { el.focus(); }
</script>

<svelte:window onkeydown={onkey} />
<div class="card">
  <label class="title" for="name">Запись сохранена. Название (необязательно):</label>
  <input id="name" type="text" bind:value={title} use:focusEl autocomplete="off" />
  {#if data.showCloudToggle}
    <label class="check">
      <input type="checkbox" bind:checked={noCloud} onchange={() => action('cloud', { allowed: !noCloud })} />
      Не отправлять в облако (без саммари от {data.provider ?? 'LLM'})
    </label>
  {/if}
  <div class="row">
    <button class="primary" onclick={submit}>Сохранить</button>
    <button onclick={() => action('skip')}>Пропустить</button>
    <span class="spacer"></span>
    <span class="sub">{left} с</span>
  </div>
  <Countdown deadlineMs={data.deadlineMs} />
</div>

<style>
  .check { display: flex; align-items: center; gap: 6px; font-size: 12px; color: var(--muted); cursor: pointer; }
</style>
