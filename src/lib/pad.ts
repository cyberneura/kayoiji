import type { Mode } from './types';

/** テキストエリアの内容。未確定文字列は `before` と `after` の間にある。 */
export interface PadText {
  before: string;
  preedit: string;
  after: string;
}

/** エンジンへ渡すキー。Rust 側の `PadKey` と同じ形。 */
export type PadKey =
  | { type: 'char'; value: string }
  | { type: 'space' }
  | { type: 'enter' }
  | { type: 'backspace' }
  | { type: 'escape' }
  | { type: 'tab' }
  | { type: 'up' }
  | { type: 'down' }
  | { type: 'left' }
  | { type: 'right' }
  | { type: 'shift_left' }
  | { type: 'shift_right' }
  | { type: 'henkan' }
  | { type: 'muhenkan' }
  | { type: 'force_japanese' }
  | { type: 'force_english' }
  | { type: 'katakana' };

/** エンジンの出力。`delete_before`、`commit`、`preedit` の順に適用する。 */
export interface PadOutput {
  consumed: boolean;
  delete_before: number;
  commit: string;
  preedit: string;
  /** 未確定文字列の中で、候補を選んでいる文節の範囲 (先頭からの文字数と長さ)。 */
  focus: [number, number] | null;
}

export interface KeyInfo {
  key: string;
  /** キーの物理的な位置。「変換」「無変換」は `key` の値が環境で違うので、こちらで見分ける。 */
  code: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  repeat: boolean;
}

/**
 * キーボードイベントをエンジンのキーに読み替える。
 * カーソル移動やショートカットなど、エンジンに渡さないキーは null を返す。
 */
export function keyFromEvent(event: KeyInfo, mode: Mode): PadKey | null {
  // 「変換」「かな」と「無変換」「英数」。
  if (event.code === 'Convert' || event.code === 'Lang1') return { type: 'henkan' };
  if (event.code === 'NonConvert' || event.code === 'Lang2') return { type: 'muhenkan' };
  if (event.metaKey || event.altKey) return null;
  if (event.ctrlKey) {
    switch (event.key) {
      case 'j':
        return { type: 'force_japanese' };
      case ';':
        return { type: 'force_english' };
      case 'k':
        return { type: 'katakana' };
      default:
        return null;
    }
  }
  switch (event.key) {
    case ' ':
      return event.shiftKey && mode === 'converting' ? { type: 'up' } : { type: 'space' };
    case 'Enter':
      return { type: 'enter' };
    case 'Backspace':
      return { type: 'backspace' };
    case 'Escape':
      return { type: 'escape' };
    case 'Tab':
      return { type: 'tab' };
    // 矢印キーは、日本語の入力中はエンジンに渡す。それ以外はカーソル移動。
    case 'ArrowUp':
      return mode === 'direct' ? null : { type: 'up' };
    case 'ArrowDown':
      return mode === 'direct' ? null : { type: 'down' };
    case 'ArrowLeft':
      if (mode === 'direct') return null;
      return event.shiftKey ? { type: 'shift_left' } : { type: 'left' };
    case 'ArrowRight':
      if (mode === 'direct') return null;
      return event.shiftKey ? { type: 'shift_right' } : { type: 'right' };
    case 'F7':
      return { type: 'katakana' };
    case 'F10':
      return { type: 'force_english' };
  }
  // 1 文字のキーだけが文字入力。"Shift" や "ArrowLeft" のような名前は文字ではない。
  if ([...event.key].length !== 1) return null;
  // キーを押し続けて同じ文字を並べるのは、日本語の入力ではない。
  if (event.repeat && mode === 'direct') return null;
  return { type: 'char', value: event.key };
}

function dropLast(text: string, count: number): string {
  if (count <= 0) return text;
  const characters = [...text];
  return characters.slice(0, Math.max(0, characters.length - count)).join('');
}

/** エンジンに渡さなかった時に、キーがテキストに対して行う編集。 */
function typed(before: string, key: PadKey): string {
  switch (key.type) {
    case 'char':
      return before + key.value;
    case 'space':
      return before + ' ';
    case 'enter':
      return before + '\n';
    case 'tab':
      return before + '\t';
    case 'backspace':
      return dropLast(before, 1);
    default:
      return before;
  }
}

/**
 * エンジンの出力をテキストに適用する。`key` は出力の元になったキーで、
 * エンジンが消費しなかった時はキー本来の編集を行う。
 */
export function applyOutput(text: PadText, output: PadOutput, key: PadKey | null): PadText {
  let before = dropLast(text.before, output.delete_before) + output.commit;
  if (!output.consumed && key) {
    before = typed(before, key);
  }
  return { before, preedit: output.preedit, after: text.after };
}

/**
 * 未確定文字列のうち、選択範囲として見せる部分を UTF-16 の位置で返す。
 * 変換中は候補を選んでいる文節、それ以外は未確定文字列の全体。
 */
export function preeditSelection(text: PadText, focus: [number, number] | null): [number, number] {
  const start = text.before.length;
  if (!focus) return [start, start + text.preedit.length];
  const characters = [...text.preedit];
  const head = characters.slice(0, focus[0]).join('').length;
  const length = characters.slice(focus[0], focus[0] + focus[1]).join('').length;
  return [start + head, start + head + length];
}

/** 未確定文字列を、表示されているとおりに確定する。 */
export function commitPreedit(text: PadText): PadText {
  return { before: text.before + text.preedit, preedit: '', after: text.after };
}
