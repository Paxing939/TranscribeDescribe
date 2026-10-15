<script lang="ts">
  import type { PromptData } from '../lib/types';
  import { action } from '../lib/api';
  import Countdown from '../lib/Countdown.svelte';
  let { data }: { data: PromptData } = $props();
  let cloudOff = $state(false);
  const cloudAllowed = $derived(!cloudOff);
  const audioDefault = $derived(data.defaultMode === 'audio');

  function record(mode: 'audio' | 'screen', screen: number) {
    action('record', { mode, screen, cloudAllowed });
  }
  function onkey(e: KeyboardEvent) {
    const tag = (e.target as HTMLElement)?.tagName;
    if (e.key === 'Escape') action('not_now');
    else if (e.key === 'Enter' && tag !== 'BUTTON') record(data.defaultMode, data.defaultScreen);
  }
</script>

<svelte:window onkeydown={onkey} />
<div class="card">
  <p class="title">Обнаружен звонок: {data.app}. Записать?</p>
  {#if data.windowTitle}<div class="sub" title={data.windowTitle}>{data.windowTitle}</div>{/if}
  <div class="row">
    <button class:primary={audioDefault} onclick={() => record('audio', data.defaultScreen)}>Только звук</button>
    {#each data.screens as n (n)}
      <button class:primary={!audioDefault && n === data.defaultScreen} onclick={() => record('screen', n)}>Экран {n}</button>
    {/each}
  </div>
  <div class="row">
    <button onclick={() => action('not_now')}>Не сейчас</button>
    <button onclick={() => action('always', { cloudAllowed })}>Всегда для {data.app}</button>
    <button class="danger" onclick={() => action('never')}>Никогда для {data.app}</button>
  </div>
  {#if data.showCloudToggle}
    <label class="check"><input type="checkbox" bind:checked={cloudOff} /> не отправлять в облако</label>
  {/if}
  {#if data.remindConsent}<div class="warn">Предупредите собеседников о записи</div>{/if}
  <Countdown deadlineMs={data.deadlineMs} />
</div>
