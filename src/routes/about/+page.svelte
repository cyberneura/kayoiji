<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import type { AppStatus } from '$lib/types';

  let status = $state<AppStatus>();

  const builtAt = $derived(
    status
      ? new Intl.DateTimeFormat('ja-JP', { dateStyle: 'medium', timeStyle: 'medium' }).format(
          new Date(status.built_at * 1000)
        )
      : ''
  );

  onMount(async () => {
    status = await invoke<AppStatus>('app_status');
  });
</script>

<main
  class="flex h-screen flex-col items-center justify-center gap-4 bg-gray-50 p-6 text-sm text-gray-900 dark:bg-gray-900 dark:text-gray-100"
>
  <header class="flex flex-col items-center gap-1">
    <h1 class="text-xl font-semibold">Kayoiji</h1>
    <span class="text-gray-500 dark:text-gray-400">通い路</span>
  </header>

  {#if status}
    <dl class="flex flex-col gap-1">
      <div class="flex gap-3">
        <dt class="w-24 shrink-0 text-gray-500 dark:text-gray-400">バージョン</dt>
        <dd>{status.version}</dd>
      </div>
      <div class="flex gap-3">
        <dt class="w-24 shrink-0 text-gray-500 dark:text-gray-400">ビルド日時</dt>
        <dd>{builtAt}</dd>
      </div>
    </dl>
  {/if}
</main>
