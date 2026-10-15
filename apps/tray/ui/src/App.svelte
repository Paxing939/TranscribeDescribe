<script lang="ts">
  import { onMount } from 'svelte';
  import { loadData, onUpdate } from './lib/api';
  import type { ViewData } from './lib/types';
  import Prompt from './views/Prompt.svelte';
  import Keep from './views/Keep.svelte';
  import Pill from './views/Pill.svelte';
  import End from './views/End.svelte';
  import Naming from './views/Naming.svelte';
  import Done from './views/Done.svelte';
  import ErrorView from './views/ErrorView.svelte';
  import Short from './views/Short.svelte';
  import History from './views/History.svelte';
  import Settings from './views/Settings.svelte';
  import { currentView } from './lib/api';

  let data = $state<ViewData | null>(null);

  onMount(() => {
    let un: (() => void) | undefined;
    let dead = false;
    loadData().then((d) => { data = d; }).catch((e) => console.error('view_data failed', e));
    onUpdate((d) => { data = d; }).then((u) => { if (dead) u(); else un = u; });
    return () => { dead = true; un?.(); };
  });
</script>

{#if currentView() === 'history'}<History />
{:else if currentView() === 'settings'}<Settings />
{:else if data}
  {#if data.view === 'prompt'}<Prompt {data} />
  {:else if data.view === 'keep'}<Keep {data} />
  {:else if data.view === 'pill'}<Pill {data} />
  {:else if data.view === 'end'}<End {data} />
  {:else if data.view === 'naming'}<Naming {data} />
  {:else if data.view === 'done'}<Done {data} />
  {:else if data.view === 'error'}<ErrorView {data} />
  {:else if data.view === 'short'}<Short {data} />
  {/if}
{/if}
