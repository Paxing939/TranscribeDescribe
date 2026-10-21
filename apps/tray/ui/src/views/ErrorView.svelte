<script lang="ts">
  import type { ErrorData } from '../lib/types';
  import { action } from '../lib/api';
  let { data }: { data: ErrorData } = $props();
  function onkey(e: KeyboardEvent) {
    if (e.key === 'Escape') action('dismiss');
  }
</script>

<svelte:window onkeydown={onkey} />
<div class="card">
  <button class="close" aria-label="Закрыть" onclick={() => action('dismiss')}>×</button>
  <p class="title" style={data.info ? '' : 'color:var(--danger)'}>{data.title}</p>
  <div class="msg">{data.message}</div>
  <div class="row">
    {#each data.actions as a, i (a.id)}
      <button class:primary={i === 0} onclick={() => action(a.id)}>{a.label}</button>
    {/each}
  </div>
</div>

<style>
  .msg { overflow: auto; white-space: pre-line; user-select: text; -webkit-user-select: text; }
</style>
