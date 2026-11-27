<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount, tick } from 'svelte';
  import EmojiGrid from '$lib/EmojiGrid.svelte';
  import { suggestion, type Review, type Spelling, type View } from '$lib/types';

  /** 入力メソッドから届く、綴りの検査の結果。 */
  interface SpellingReport {
    spelling: Spelling | null;
    /** 言語モデルが使えるかどうか。使えなければ、訂正文と訳は届かない。 */
    review_available: boolean;
  }

  let view = $state<View>({
    input_mode: 'japanese',
    forced: false,
    mode: 'direct',
    reading: '',
    candidates: [],
    selected: null,
    page: 0,
    page_count: 0,
    emoji: null
  });
  let panel = $state<HTMLElement>();
  let report = $state<SpellingReport | null>(null);
  let review = $state<Review | null>(null);
  // 言語モデルが答えられなかった。答えを待つ表示のままにしない。
  let reviewFailed = $state(false);
  const correction = $derived(suggestion(report?.spelling ?? null, review));

  // 表示を更新してから大きさを測り、ウインドウを内容に合わせてもらう。
  // ウインドウは、この報告を受けた時点で表示される。
  async function fit() {
    await tick();
    if (!panel) return;
    const rect = panel.getBoundingClientRect();
    await invoke('overlay_resize', {
      width: Math.ceil(rect.width),
      height: Math.ceil(rect.height)
    });
  }

  function show(next: View) {
    // 英文が変わったら、前の英文に対する結果を消す。新しい結果は、この後に届く。
    if (next.mode !== 'checking' || next.reading !== view.reading) {
      report = null;
      review = null;
      reviewFailed = false;
    }
    view = next;
    // 隠した後に届く空の内容では、大きさを報告しない。次の打鍵で表示が求められた後に報告が届くと、
    // 案内だけのウインドウが一瞬出る。
    if (next.mode === 'direct' || (next.mode === 'checking' && !next.reading)) return;
    return fit();
  }

  onMount(() => {
    const unlisten = [
      listen<View>('view', (event) => show(event.payload)),
      listen<SpellingReport>('english-spelling', (event) => {
        report = event.payload;
        fit();
      }),
      listen<Review>('english-review', (event) => {
        review = event.payload;
        fit();
      }),
      listen('english-review-failed', () => {
        reviewFailed = true;
        fit();
      })
    ];
    invoke<View>('current_view').then(show);
    return () => {
      for (const stop of unlisten) stop.then((stop) => stop());
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
  {:else if view.mode === 'checking'}
    <dl class="flex max-w-lg flex-col gap-1 px-3 py-2 text-xs whitespace-normal">
      {#if report?.spelling && report.spelling.misspellings.length > 0}
        <div class="flex gap-2">
          <dt class="w-8 shrink-0 text-gray-500 dark:text-gray-400">綴り</dt>
          <dd class="flex flex-wrap gap-x-3 gap-y-1">
            {#each report.spelling.misspellings as misspelling}
              <span>
                <span class="text-red-600 line-through dark:text-red-400">{misspelling.word}</span>
                {misspelling.guesses.length > 0 ? misspelling.guesses.join(' / ') : '候補なし'}
              </span>
            {/each}
          </dd>
        </div>
      {/if}
      {#if report?.review_available}
        <div class="flex gap-2">
          <dt class="w-8 shrink-0 text-gray-500 dark:text-gray-400">英文</dt>
          <dd>
            {#if review?.unchanged}
              自然な英語です
            {:else if review}
              {review.corrected}
            {:else if reviewFailed}
              <span class="text-gray-500 dark:text-gray-400">訂正文を作れませんでした</span>
            {:else}
              <span class="text-gray-500 dark:text-gray-400">…</span>
            {/if}
          </dd>
        </div>
        {#if review}
          <div class="flex gap-2">
            <dt class="w-8 shrink-0 text-gray-500 dark:text-gray-400">訳</dt>
            <dd>{review.translation}</dd>
          </div>
        {/if}
      {/if}
      <div class="flex gap-2 text-gray-500 dark:text-gray-400">
        {#if correction}<span>Tab 訂正</span>{/if}
        <span>Enter 確定</span>
        <span>Esc 取消</span>
      </div>
    </dl>
  {:else if view.mode === 'emoji'}
    <EmojiGrid {view} />
  {:else if view.mode === 'composing'}
    <div class="flex items-center gap-2 px-2 py-1 text-xs text-gray-500 dark:text-gray-400">
      <span>Space 変換</span>
      <span>Enter 確定</span>
      <span>{view.input_mode === 'japanese' ? 'Esc 取消' : 'Esc 英字'}</span>
    </div>
  {/if}
</div>
