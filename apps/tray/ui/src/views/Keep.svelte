<script lang="ts">
  import type { KeepData } from '../lib/types';
  import { action } from '../lib/api';
  let { data }: { data: KeepData } = $props();
</script>

<div class="card">
  <p class="title">Запись уже идёт ({data.app}) — оставить?</p>
  <div class="row">
    <button class="primary" onclick={() => action('keep')}>Оставить</button>
    <button class="danger" onclick={() => action('discard')}>Удалить</button>
  </div>
</div>
