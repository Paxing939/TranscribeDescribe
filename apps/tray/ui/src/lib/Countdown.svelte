<script lang="ts">
  import { clock } from './clock.svelte';
  let { deadlineMs }: { deadlineMs: number } = $props();
  // total span is fixed when a deadline is first seen
  let total = $state(1);
  let seen = 0;
  $effect.pre(() => {
    if (deadlineMs !== seen) {
      seen = deadlineMs;
      total = Math.max(1, deadlineMs - Date.now());
    }
  });
  const pct = $derived(Math.max(0, Math.min(100, ((deadlineMs - clock.now) / total) * 100)));
</script>

<div class="bar" role="progressbar" aria-label="Осталось времени" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(pct)}>
  <i style="width:{pct}%"></i>
</div>
