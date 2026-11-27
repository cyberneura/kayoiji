<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import {
    adoptNativeEdit,
    applyOutput,
    commitPreedit,
    keyFromEvent,
    preeditSelection,
    type PadKey,
    type PadOutput,
    type PadText
  } from '$lib/pad';
  import { suggestion, type Review, type Spelling, type View } from '$lib/types';

  interface PadResponse extends PadOutput {
    view: View;
    timeout_ms: number | null;
  }

  /** 自動切り替えの設定を覚えておく localStorage のキー。 */
  const AUTO_SWITCH_KEY = 'kayoiji-pad-auto-switch';

  const idle: View = {
    input_mode: 'japanese',
    forced: false,
    mode: 'direct',
    reading: '',
    candidates: [],
    selected: null,
    page: 0,
    page_count: 0
  };

  let textarea = $state<HTMLTextAreaElement>();
  let text: PadText = { before: '', preedit: '', after: '' };
  let focus: [number, number] | null = null;
  let view = $state<View>(idle);
  let systemInputMethod = $state(false);
  let autoSwitch = $state(false);
  let copied = $state(false);
  // 打鍵は届いた順に処理する。エンジンの応答を待つ間に次の打鍵が来ても、順序を入れ替えない。
  let queue: Promise<unknown> = Promise.resolve();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;

  /** 打鍵がこの時間 (ミリ秒) 止まったら、綴りの訂正候補を作り、言語モデルに訂正文と訳を頼む。 */
  const CHECK_DELAY_MS = 500;
  let spelling = $state<Spelling | null>(null);
  let review = $state<Review | null>(null);
  let reviewError = $state('');
  // 最後に頼んだ依頼の番号。打鍵が進んだ後に届いた、前の英文への答えを捨てる。
  let reviewId = 0;
  let reviewTimer: ReturnType<typeof setTimeout> | undefined;
  const correction = $derived(suggestion(spelling, review));

  /** 英文チェックで受け取っている英文が変わったら、綴りを調べ直し、訂正文と訳を頼み直す。 */
  function refreshCheck() {
    clearTimeout(reviewTimer);
    // 前の英文に対する結果は、ここで消す。残すと、新しい結果が届くまでの間に押した Tab が、
    // 前の英文の訂正案で今の英文を置き換える。
    spelling = null;
    review = null;
    reviewError = '';
    reviewId += 1;
    const text = view.mode === 'checking' ? view.reading : '';
    if (!text.trim()) return;
    const id = reviewId;
    const check = async (lookup: boolean) => {
      const result = await invoke<Spelling>('pad_spelling', { text, lookup });
      if (id === reviewId) spelling = result;
      return result;
    };
    // 打鍵のたびには、すでに調べた単語の訂正候補だけを付ける。新しい候補を作るのは時間がかかる。
    check(false).catch((cause) => console.error(cause));
    reviewTimer = setTimeout(() => {
      invoke('pad_review', { id, text }).catch((cause) => {
        if (id === reviewId) reviewError = String(cause);
      });
      // 言語モデルが使えなくても、綴りの訂正候補は最後まで作る。
      const completeSpelling = async () => {
        while (id === reviewId && (await check(true)).pending);
      };
      completeSpelling().catch((cause) => console.error(cause));
    }, CHECK_DELAY_MS);
  }

  /**
   * 英文チェックで受け取っている英文を、訂正案に置き換える。訂正案は、先に押したキーの処理が
   * 終わってから読む。押した時点の訂正案を使うと、その後に処理される打鍵を消してしまう。
   */
  function applyCorrection() {
    enqueue(async () => {
      const text = correction;
      if (!text || view.mode !== 'checking') return;
      receive(await invoke<PadResponse>('pad_replace_english', { text }), null);
    });
  }

  const modeLabel = $derived(
    systemInputMethod
      ? 'OS'
      : view.input_mode === 'direct'
        ? 'A'
        : view.input_mode === 'english'
          ? '英文'
          : view.input_mode === 'japanese' || view.forced
          ? 'あ'
          : '自動'
  );
  const modeTitle = $derived(
    systemInputMethod
      ? 'OS の入力メソッドで入力しています。内蔵のエンジンを試す時は、入力ソースを英字にしてください。'
      : view.input_mode === 'english'
        ? '英文チェック。Enter で確定、Esc で取り消し。何も打っていない時の Esc か「英数」で直接入力にします。'
        : view.input_mode === 'direct'
        ? `直接入力。「かな」を続けて 2 度押すと英文チェック。クリックか「かな」で${autoSwitch ? '自動切り替え' : '日本語入力'}に戻します。`
        : view.input_mode === 'japanese'
          ? '日本語入力。クリックか「英数」で直接入力にします。'
          : view.forced
            ? '次の入力を日本語として始めます。'
            : '自動切り替え。クリックか「英数」で直接入力にします。'
  );

  function render() {
    if (!textarea) return;
    textarea.value = text.before + text.preedit + text.after;
    // 未確定文字列は選択範囲として見せる。変換中は、候補を選んでいる文節だけを選択する。
    const [start, end] = preeditSelection(text, focus);
    textarea.setSelectionRange(start, end);
  }

  /** 未確定文字列が無い時は、利用者が動かしたカーソルの位置から入力を続ける。 */
  function syncFromTextarea(key: PadKey) {
    if (!textarea || text.preedit) return;
    const { value, selectionStart, selectionEnd } = textarea;
    const replacesSelection = ['char', 'space', 'enter', 'tab'].includes(key.type);
    text = {
      before: value.slice(0, selectionStart),
      preedit: '',
      after: value.slice(replacesSelection ? selectionEnd : selectionStart)
    };
  }

  /** 処理を順番待ちに入れる。エンジンの呼び出しが失敗しても、後に続く打鍵の処理は止めない。 */
  function enqueue(task: () => Promise<void>) {
    queue = queue.then(task).catch((cause) => console.error(cause));
  }

  function receive(response: PadResponse, key: PadKey | null) {
    if (textarea) {
      text = adoptNativeEdit(text, textarea.value, textarea.selectionStart, textarea.selectionEnd);
    }
    text = applyOutput(text, response, key);
    focus = response.focus;
    view = response.view;
    render();
    scheduleTimeout(response.timeout_ms);
    refreshCheck();
  }

  function scheduleTimeout(timeoutMs: number | null) {
    clearTimeout(timer);
    if (timeoutMs === null) return;
    timer = setTimeout(() => {
      enqueue(async () => receive(await invoke<PadResponse>('pad_timeout'), null));
    }, timeoutMs + 1);
  }

  function send(key: PadKey) {
    enqueue(async () => {
      syncFromTextarea(key);
      receive(await invoke<PadResponse>('pad_key', { key }), key);
    });
  }

  /**
   * 未確定文字列を表示のまま確定して、エンジンを最初の状態に戻す。
   * `reason` は、きっかけになった操作。意図しない確定の原因を調べるために記録する。
   */
  function release(reason: string) {
    clearTimeout(timer);
    enqueue(async () => {
      text = commitPreedit(text);
      focus = null;
      view = await invoke<View>('pad_reset', { reason });
      refreshCheck();
    });
  }

  function onkeydown(event: KeyboardEvent) {
    // OS の入力メソッドが有効な間は、入力を OS に任せる。
    if (systemInputMethod || event.isComposing || event.key === 'Process') return;
    if (event.key === 'Tab' && view.mode === 'checking' && correction) {
      event.preventDefault();
      applyCorrection();
      return;
    }
    const key = keyFromEvent(event, view.mode);
    if (!key) {
      // 修飾キーだけの打鍵では、入力を区切らない。
      if (!['Shift', 'Control', 'Alt', 'Meta', 'CapsLock'].includes(event.key)) {
        release(`key ${event.key}`);
      }
      return;
    }
    const hasSelection = textarea !== undefined && textarea.selectionStart !== textarea.selectionEnd;
    if (key.type === 'backspace' && hasSelection && !text.preedit) {
      // 選択範囲の削除は、テキストエリアに任せる。
      release('delete selection');
      return;
    }
    event.preventDefault();
    send(key);
  }

  function onkeyup() {
    // 入力ソースは、キー操作 (英数・かな、Ctrl + Space など) で切り替わる。少し待ってから調べ直す。
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(refreshInputSource, 150);
  }

  async function refreshInputSource() {
    systemInputMethod = await invoke<boolean>('pad_system_input_method');
  }

  function toggleMode() {
    if (systemInputMethod) return;
    send({ type: view.input_mode === 'direct' ? 'henkan' : 'muhenkan' });
    textarea?.focus();
  }

  function setAutoSwitch(enabled: boolean) {
    enqueue(async () => {
      // 表示と保存は、エンジンが切り替わってから変える。呼び出しが失敗した時に、表示だけが
      // 変わったままにならないようにする。
      const response = await invoke<PadResponse>('pad_set_auto_switch', { enabled });
      autoSwitch = enabled;
      try {
        localStorage.setItem(AUTO_SWITCH_KEY, enabled ? '1' : '0');
      } catch {
        // 保存できない環境 (プライベートウインドウなど) では、次の起動で既定に戻るだけ。
      }
      if (text.preedit) {
        receive(response, null);
        return;
      }
      // 確定する文字列が無い時は、テキストエリアを描き直さない。描き直すと、利用者がクリックで
      // 動かしたカーソルや選択範囲が、こちらが覚えている位置に戻ってしまう。
      view = response.view;
      scheduleTimeout(response.timeout_ms);
    });
    textarea?.focus();
  }

  function savedAutoSwitch(): boolean {
    try {
      return localStorage.getItem(AUTO_SWITCH_KEY) === '1';
    } catch {
      return false;
    }
  }

  async function copy() {
    await navigator.clipboard.writeText(textarea?.value ?? '');
    copied = true;
    setTimeout(() => (copied = false), 1200);
  }

  onMount(() => {
    refreshInputSource();
    // 画面を読み込み直した時に、エンジンに前の入力の状態が残らないようにする。
    release('page load');
    setAutoSwitch(savedAutoSwitch());
    window.addEventListener('focus', refreshInputSource);
    const unlisten = [
      listen<Review>('english-review', (event) => {
        if (event.payload.id === reviewId) {
          review = event.payload;
          reviewError = '';
        }
      }),
      listen<number>('english-review-failed', (event) => {
        if (event.payload === reviewId && !review) reviewError = '訂正文を作れませんでした';
      })
    ];
    textarea?.focus();
    return () => {
      window.removeEventListener('focus', refreshInputSource);
      for (const stop of unlisten) stop.then((stop) => stop());
    };
  });
</script>

<main class="flex h-screen flex-col gap-2 bg-gray-50 p-3 text-sm text-gray-900 dark:bg-gray-900 dark:text-gray-100">
  <textarea
    bind:this={textarea}
    data-annotate="input-pad"
    class="min-h-0 flex-1 resize-none rounded border border-gray-300 bg-white p-3 text-base leading-7 outline-none focus:border-indigo-500 dark:border-gray-600 dark:bg-gray-800"
    spellcheck="false"
    autocomplete="off"
    autocapitalize="off"
    {onkeydown}
    {onkeyup}
    onmousedown={() => release('mouse')}
    oninput={(event) => release(`input ${event instanceof InputEvent ? event.inputType : ''}`)}
  ></textarea>

  <div class="flex min-h-7 items-center gap-3">
    <div class="flex min-w-0 flex-1 items-center">
      {#if view.mode === 'converting'}
        <ol class="flex flex-wrap items-center gap-x-3 gap-y-1">
          {#each view.candidates as candidate, index}
            <li
              class="flex items-center gap-1 rounded px-1.5 py-0.5 {index === view.selected
                ? 'bg-indigo-600 text-white'
                : ''}"
            >
              <span class="text-xs opacity-60">{index + 1}</span>
              <span>{candidate}</span>
            </li>
          {/each}
          <li class="text-xs text-gray-500 dark:text-gray-400">{view.page + 1}/{view.page_count}</li>
        </ol>
      {:else if view.mode === 'checking' && view.reading.trim()}
        <dl data-annotate="panel-english-check" class="flex flex-col gap-1 text-xs">
          {#if spelling && spelling.misspellings.length > 0}
            <div class="flex gap-2">
              <dt class="w-10 shrink-0 text-gray-500 dark:text-gray-400">綴り</dt>
              <dd class="flex flex-wrap gap-x-3 gap-y-1">
                {#each spelling.misspellings as misspelling}
                  <span>
                    <span class="text-red-600 line-through dark:text-red-400">{misspelling.word}</span>
                    {misspelling.guesses.length > 0 ? misspelling.guesses.join(' / ') : '候補なし'}
                  </span>
                {/each}
              </dd>
            </div>
          {/if}
          <div class="flex gap-2">
            <dt class="w-10 shrink-0 text-gray-500 dark:text-gray-400">英文</dt>
            <dd>
              {#if reviewError}
                <span class="text-gray-500 dark:text-gray-400">{reviewError}</span>
              {:else if !review}
                <span class="text-gray-500 dark:text-gray-400">…</span>
              {:else if review.unchanged}
                自然な英語です
              {:else}
                {review.corrected}
              {/if}
            </dd>
          </div>
          {#if review}
            <div class="flex gap-2">
              <dt class="w-10 shrink-0 text-gray-500 dark:text-gray-400">訳</dt>
              <dd>{review.translation}</dd>
            </div>
          {/if}
          {#if correction}
            <p class="text-gray-500 dark:text-gray-400">Tab で訂正案に置き換える / Enter でこのまま確定</p>
          {/if}
        </dl>
      {:else}
        <p class="text-xs text-gray-500 dark:text-gray-400">{modeTitle}</p>
      {/if}
    </div>
    <button
      data-annotate="button-auto-switch"
      class="flex h-7 shrink-0 items-center rounded border px-2 text-xs {autoSwitch
        ? 'border-indigo-600 bg-indigo-600 text-white'
        : 'border-gray-300 hover:bg-gray-100 dark:border-gray-600 dark:hover:bg-gray-800'}"
      title="英語と日本語を自動で切り替える"
      aria-pressed={autoSwitch}
      onclick={() => setAutoSwitch(!autoSwitch)}
    >
      自動切り替え
    </button>
    <button
      data-annotate="button-mode"
      class="flex h-7 min-w-9 shrink-0 items-center justify-center rounded border border-gray-300 px-2 text-xs font-medium hover:bg-gray-100 dark:border-gray-600 dark:hover:bg-gray-800"
      title={modeTitle}
      onclick={toggleMode}
    >
      {modeLabel}
    </button>
    <button
      data-annotate="button-copy"
      class="flex h-7 shrink-0 items-center rounded border border-gray-300 px-2 text-xs hover:bg-gray-100 dark:border-gray-600 dark:hover:bg-gray-800"
      onclick={copy}
    >
      {copied ? 'コピーしました' : 'コピー'}
    </button>
  </div>
</main>
