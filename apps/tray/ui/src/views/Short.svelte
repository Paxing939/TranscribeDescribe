<script lang="ts">
  import type { ShortData } from '../lib/types';
  import { action } from '../lib/api';
  let { data }: { data: ShortData } = $props();
</script>

<div class="card">
  <p class="title">Запись короче минуты ({data.durationSec} с) — удалить?</p>
  <div class="row">
    <button class="danger solid" onclick={() => action('delete')}>Удалить</button>
    <button onclick={() => action('keep')}>Оставить</button>
  </div>
</div>
