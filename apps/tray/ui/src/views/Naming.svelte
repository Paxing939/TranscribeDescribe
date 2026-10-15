<script lang="ts">
  import type { NamingData } from '../lib/types';
  import { action } from '../lib/api';
  import { secsLeft } from '../lib/clock.svelte';
  import Countdown from '../lib/Countdown.svelte';
  let { data }: { data: NamingData } = $props();
  let title = $state('');
  const left = $derived(secsLeft(data.deadlineMs));
  const submit = () => action('submit', { title: title.trim() });
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape') action('skip');
    else if (e.key === 'Enter' && (e.target as HTMLElement)?.tagName !== 'BUTTON') submit();
  }
  function focusEl(el: HTMLInputElement) { el.focus(); }
</script>

<svelte:window onkeydown={onkey} />
<div class="card">
  <label class="title" for="name">Запись сохранена. Название (необязательно):</label>
  <input id="name" type="text" bind:value={title} use:focusEl autocomplete="off" />
  <div class="row">
    <button class="primary" onclick={submit}>Сохранить</button>
    <button onclick={() => action('skip')}>Пропустить</button>
    <span class="spacer"></span>
    <span class="sub">{left} с</span>
  </div>
  <Countdown deadlineMs={data.deadlineMs} />
</div>
