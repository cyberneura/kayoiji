<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount, tick } from 'svelte';
  import type { View } from '$lib/types';

  let view = $state<View>({
    input_mode: 'auto',
    forced: false,
    mode: 'direct',
    reading: '',
    candidates: [],
    selected: null,
    page: 0,
    page_count: 0
  });
  let panel = $state<HTMLElement>();

  // 表示を更新してから大きさを測り、ウインドウを内容に合わせてもらう。
  // ウインドウは、この報告を受けた時点で表示される。
  async function show(next: View) {
    view = next;
    await tick();
    if (!panel) return;
    const rect = panel.getBoundingClientRect();
    await invoke('overlay_resize', {
      width: Math.ceil(rect.width),
      height: Math.ceil(rect.height)
    });
  }

  onMount(() => {
    const unlisten = listen<View>('view', (event) => show(event.payload));
    invoke<View>('current_view').then(show);
    return () => {
      unlisten.then((stop) => stop());
    };
  });
</script>

<div
  bind:this={panel}
  class="flex w-max flex-col bg-white text-sm whitespace-nowrap text-gray-900 select-none dark:bg-gray-800 dark:text-gray-100"
>
  {#if view.mode === 'converting'}
    <ol class="flex flex-col py-1">
      {#each view.candidates as candidate, index}
        <li
          class="flex items-center gap-2 px-3 py-0.5 {index === view.selected
            ? 'bg-indigo-600 text-white'
            : ''}"
        >
          <span class="w-3 text-xs opacity-60">{index + 1}</span>
          <span>{candidate}</span>
        </li>
      {/each}
    </ol>
    <div
      class="flex items-center justify-between gap-4 border-t border-gray-200 px-3 py-1 text-xs text-gray-500 dark:border-gray-700 dark:text-gray-400"
    >
      <span>{view.reading}</span>
      <span>{view.page + 1}/{view.page_count}</span>
    </div>
  {:else if view.mode === 'composing'}
    <div class="flex items-center gap-2 px-2 py-1 text-xs text-gray-500 dark:text-gray-400">
      <span>Space 変換</span>
      <span>Enter 確定</span>
      <span>Esc 英字</span>
    </div>
  {/if}
</div>
