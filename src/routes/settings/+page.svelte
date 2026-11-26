<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import type { AppStatus } from '$lib/types';

  let status = $state<AppStatus>();
  let error = $state('');

  const commonKeys = [
    ['Space', '変換する / 次の候補'],
    ['Enter', '確定する'],
    ['← / →', '変換中: 候補を選ぶ文節を移る'],
    ['Shift + ← / →', '変換中: 文節を縮める / 伸ばす'],
    ['1 - 9', '候補を番号で選ぶ'],
    ['Ctrl + K / F7', 'カタカナにする'],
    ['英数 / 無変換', '直接入力にする (すべてのキーをそのまま入力する)']
  ];
  const japaneseKeys = [
    ['かな / 変換', '日本語入力にする'],
    ['Esc', 'かなの入力中: 入力を取り消す。変換中: かなに戻る'],
    ['Ctrl + ; / F10', 'かなの入力中でも変換中でも、打ったままの英字に戻す']
  ];
  const autoKeys = [
    ['かな / 変換', '自動切り替えにする。かなの入力中は変換する。入力中でなければ、次の入力を日本語として始める'],
    ['Esc', 'かなの入力中: 打ったままの英字に戻す (その単語を英語として覚える)。変換中: かなに戻る'],
    ['Ctrl + ; / F10', 'かなの入力中でも変換中でも、打ったままの英字に戻す (その単語を英語として覚える)'],
    ['Ctrl + J', '直前の単語を日本語として読み直す (その単語を日本語として覚える)']
  ];
  const keys = $derived([...commonKeys, ...(status?.settings.auto_switch ? autoKeys : japaneseKeys)]);

  async function refresh() {
    status = await invoke<AppStatus>('app_status');
  }

  async function save() {
    if (!status) return;
    error = '';
    try {
      await invoke('update_settings', { settings: status.settings });
    } catch (cause) {
      error = String(cause);
    }
    await refresh();
  }

  onMount(() => {
    refresh();
    // 入力メニューから設定を変えた時に、表示を合わせる。
    const unlisten = listen('settings-changed', refresh);
    return () => {
      unlisten.then((stop) => stop());
    };
  });
</script>

<main
  class="flex min-h-screen flex-col gap-6 bg-gray-50 p-6 text-sm text-gray-900 dark:bg-gray-900 dark:text-gray-100"
>
  <header class="flex items-baseline gap-3">
    <h1 class="text-xl font-semibold">Kayoiji</h1>
    <span class="text-gray-500 dark:text-gray-400">通い路</span>
    {#if status}
      <span class="text-xs text-gray-400">v{status.version}</span>
    {/if}
  </header>

  {#if status}
    {#if !status.input_method_running}
      <section
        class="flex flex-col gap-2 rounded-lg border border-amber-300 bg-amber-50 p-4 dark:border-amber-700 dark:bg-amber-950"
      >
        <p class="font-medium">入力メソッドとして起動されていません</p>
        <p>
          Kayoiji.app を「~/Library/Input Methods」に置き、「システム設定 → キーボード → 入力ソース → 編集 →
          +」の「日本語」から Kayoiji を追加してください。
        </p>
      </section>
    {/if}

    {#if status.warnings.length > 0}
      <section
        class="flex flex-col gap-2 rounded-lg border border-amber-300 bg-amber-50 p-4 dark:border-amber-700 dark:bg-amber-950"
      >
        <p class="font-medium">読み込めなかったファイルがあります。既定の内容で動いています</p>
        {#each status.warnings as warning}
          <p class="font-mono text-xs break-all">{warning}</p>
        {/each}
      </section>
    {/if}

    <section class="flex flex-col gap-4 rounded-lg bg-white p-4 shadow-sm dark:bg-gray-800">
      <label class="flex items-start gap-2">
        <input
          data-annotate="input-auto-switch"
          type="checkbox"
          class="mt-0.5"
          bind:checked={status.settings.auto_switch}
          onchange={save}
        />
        <span class="flex flex-col gap-1">
          <span>英語と日本語を自動で切り替える</span>
          <span class="text-xs text-gray-500 dark:text-gray-400">
            打った単語が英語か日本語かを判定して、キーで切り替えずに入力します。無効の時は、「かな」キーで日本語入力、「英数」キーで直接入力にします。
          </span>
        </span>
      </label>

      <label class="flex flex-col gap-1 {status.settings.auto_switch ? '' : 'opacity-50'}">
        <span>ひと続きの入力とみなす打鍵の間隔 (ミリ秒)</span>
        <div class="flex items-center gap-3">
          <input
            data-annotate="input-sequence-timeout"
            type="number"
            min="50"
            max="2000"
            step="10"
            class="w-28 rounded border border-gray-300 bg-white px-2 py-1 dark:border-gray-600 dark:bg-gray-700"
            bind:value={status.settings.sequence_timeout_ms}
            onchange={save}
          />
          <span class="text-xs text-gray-500 dark:text-gray-400">
            自動切り替えで使います。これより間隔が空いた単語は、英語として確定します。
          </span>
        </div>
      </label>

      {#if error}
        <p class="text-red-600">{error}</p>
      {/if}
    </section>

    <section class="flex flex-col gap-2 rounded-lg bg-white p-4 shadow-sm dark:bg-gray-800">
      <h2 class="font-medium">キー操作</h2>
      <dl class="flex flex-col gap-1">
        {#each keys as [key, description]}
          <div class="flex gap-3">
            <dt class="w-44 shrink-0 font-mono text-xs leading-5 text-gray-600 dark:text-gray-300">
              {key}
            </dt>
            <dd>{description}</dd>
          </div>
        {/each}
      </dl>
    </section>

    <section class="flex flex-col gap-1 text-xs text-gray-500 dark:text-gray-400">
      <p>
        自動切り替えで英語か日本語かを決め打ちする規則は、次のファイルに追加できます
        (入力メソッドの再起動で反映)。
      </p>
      <p class="font-mono break-all">{status.rules_path}</p>
    </section>
  {/if}
</main>
