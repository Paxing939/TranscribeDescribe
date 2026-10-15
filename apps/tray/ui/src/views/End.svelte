<script lang="ts">
  import type { EndData } from '../lib/types';
  import { action } from '../lib/api';
  import { secsLeft } from '../lib/clock.svelte';
  import Countdown from '../lib/Countdown.svelte';
  let { data }: { data: EndData } = $props();
  const texts = {
    signal_lost: 'Похоже, звонок закончился.',
    silence: 'Давно тишина — звонок закончился?',
    max_duration: 'Запись идёт очень долго.',
  } as const;
  const left = $derived(secsLeft(data.deadlineMs));
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape') action('continue');
  }
</script>

<svelte:window onkeydown={onkey} />
<div class="card">
  <p class="title">{texts[data.reason]}</p>
  <div class="sub">Остановлю через {left} с</div>
  <div class="row">
    <button class="primary" onclick={() => action('stop')}>Остановить</button>
    <button onclick={() => action('continue')}>Продолжить</button>
  </div>
  <Countdown deadlineMs={data.deadlineMs} />
</div>
