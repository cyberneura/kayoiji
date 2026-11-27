<script lang="ts">
  import { visibleRows } from '$lib/emoji';
  import type { View } from '$lib/types';

  let { view }: { view: View } = $props();

  const emoji = $derived(view.emoji);
  const shown = $derived(visibleRows(view.candidates, emoji?.columns ?? 10, view.selected));
</script>

{#if emoji}
  <div data-annotate="panel-emoji" class="flex flex-col gap-1 px-2 py-1.5 text-xs">
    <div class="flex items-center gap-2">
      {#if emoji.group === null}
        <span class="text-gray-500 dark:text-gray-400">検索</span>
        <span class="text-sm">{emoji.query}</span>
      {:else}
        <span class="text-sm">{emoji.groups[emoji.group]}</span>
        <span class="text-gray-500 dark:text-gray-400">{emoji.group + 1}/{emoji.groups.length}</span>
      {/if}
    </div>
    <div class="flex flex-col">
      {#each shown.rows as row, offset}
        <div class="flex">
          {#each row as text, column}
            {@const index = (shown.start + offset) * emoji.columns + column}
            <span
              class="flex h-8 w-8 items-center justify-center rounded text-xl {index === view.selected
                ? 'bg-indigo-600'
                : ''}">{text}</span
            >
          {/each}
        </div>
      {:else}
        <span class="px-1 py-2 text-gray-500 dark:text-gray-400">見つかりません</span>
      {/each}
    </div>
    {#if emoji.variants.length > 0}
      <div class="flex max-w-80 flex-wrap border-t border-gray-200 pt-1 dark:border-gray-700">
        {#each emoji.variants as text, index}
          <span
            class="flex h-8 w-8 items-center justify-center rounded text-xl {index === emoji.variant
              ? 'bg-indigo-600'
              : ''}">{text}</span
          >
        {/each}
      </div>
    {/if}
    <div class="flex items-center gap-2 text-gray-500 dark:text-gray-400">
      <span class="min-w-0 flex-1 truncate">{emoji.name}</span>
      <span>Shift+←→ 分類</span>
      <span>Tab 肌の色</span>
      <span>Enter 確定</span>
      <span>Esc やめる</span>
    </div>
  </div>
{/if}
