import { describe, expect, it } from 'vitest';
import {
  adoptNativeEdit,
  applyOutput,
  commitPreedit,
  keyFromEvent,
  preeditSelection,
  type KeyInfo,
  type PadText
} from './pad';

function event(key: string, modifiers: Partial<KeyInfo> = {}): KeyInfo {
  return {
    key,
    code: '',
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    repeat: false,
    ...modifiers
  };
}

const empty: PadText = { before: '', preedit: '', after: '' };

describe('keyFromEvent', () => {
  it('maps characters and editing keys', () => {
    expect(keyFromEvent(event('a'), 'direct')).toEqual({ type: 'char', value: 'a' });
    expect(keyFromEvent(event('A', { shiftKey: true }), 'direct')).toEqual({ type: 'char', value: 'A' });
    expect(keyFromEvent(event(' '), 'direct')).toEqual({ type: 'space' });
    expect(keyFromEvent(event('Enter'), 'composing')).toEqual({ type: 'enter' });
    expect(keyFromEvent(event('Backspace'), 'composing')).toEqual({ type: 'backspace' });
    expect(keyFromEvent(event('Escape'), 'composing')).toEqual({ type: 'escape' });
  });

  it('maps the language shortcuts', () => {
    expect(keyFromEvent(event('j', { ctrlKey: true }), 'direct')).toEqual({ type: 'force_japanese' });
    expect(keyFromEvent(event(';', { ctrlKey: true }), 'composing')).toEqual({ type: 'force_english' });
    expect(keyFromEvent(event('k', { ctrlKey: true }), 'composing')).toEqual({ type: 'katakana' });
    expect(keyFromEvent(event('F10'), 'composing')).toEqual({ type: 'force_english' });
  });

  it('leaves shortcuts and named keys to the text area', () => {
    expect(keyFromEvent(event('c', { metaKey: true }), 'composing')).toBeNull();
    expect(keyFromEvent(event('a', { altKey: true }), 'direct')).toBeNull();
    expect(keyFromEvent(event('c', { ctrlKey: true }), 'direct')).toBeNull();
    expect(keyFromEvent(event('Shift'), 'direct')).toBeNull();
    expect(keyFromEvent(event('ArrowLeft'), 'direct')).toBeNull();
  });

  it('maps the conversion keys by their physical position', () => {
    expect(keyFromEvent(event('Convert', { code: 'Convert' }), 'direct')).toEqual({ type: 'henkan' });
    expect(keyFromEvent(event('KanaMode', { code: 'Lang1' }), 'direct')).toEqual({ type: 'henkan' });
    expect(keyFromEvent(event('NonConvert', { code: 'NonConvert' }), 'composing')).toEqual({
      type: 'muhenkan'
    });
    expect(keyFromEvent(event('Eisu', { code: 'Lang2' }), 'composing')).toEqual({ type: 'muhenkan' });
  });

  it('sends the arrow keys to the engine only during Japanese input', () => {
    expect(keyFromEvent(event('ArrowDown'), 'converting')).toEqual({ type: 'down' });
    expect(keyFromEvent(event('ArrowUp'), 'converting')).toEqual({ type: 'up' });
    expect(keyFromEvent(event('ArrowLeft'), 'converting')).toEqual({ type: 'left' });
    expect(keyFromEvent(event('ArrowRight'), 'composing')).toEqual({ type: 'right' });
    expect(keyFromEvent(event('ArrowLeft', { shiftKey: true }), 'converting')).toEqual({
      type: 'shift_left'
    });
    expect(keyFromEvent(event('ArrowRight', { shiftKey: true }), 'converting')).toEqual({
      type: 'shift_right'
    });
    expect(keyFromEvent(event('ArrowDown'), 'direct')).toBeNull();
    expect(keyFromEvent(event(' ', { shiftKey: true }), 'converting')).toEqual({ type: 'up' });
    expect(keyFromEvent(event(' ', { shiftKey: true }), 'direct')).toEqual({ type: 'space' });
  });

  it('does not treat a held letter as a word outside a composition', () => {
    expect(keyFromEvent(event('a', { repeat: true }), 'direct')).toBeNull();
    expect(keyFromEvent(event('a', { repeat: true }), 'composing')).toEqual({ type: 'char', value: 'a' });
  });
});

describe('adoptNativeEdit', () => {
  it('keeps the text when the textarea shows what was rendered', () => {
    const text: PadText = { before: 'ab', preedit: 'にほん', after: 'z' };
    expect(adoptNativeEdit(text, 'abにほんz', 2, 5)).toBe(text);
  });

  it('continues from a paste made while a response was pending', () => {
    // 'c' の応答を待つ間に、"ab" の後ろへ "XYZ" を貼り付けた。
    const text: PadText = { before: 'ab', preedit: '', after: '' };
    const passed = { consumed: false, delete_before: 0, commit: '', preedit: '', focus: null };

    const adopted = adoptNativeEdit(text, 'abXYZ', 5, 5);

    expect(adopted).toEqual({ before: 'abXYZ', preedit: '', after: '' });
    // 応答を適用しても、貼り付けた文字列は残る。
    expect(applyOutput(adopted, passed, { type: 'char', value: 'c' }).before).toBe('abXYZc');
  });
});

describe('applyOutput', () => {
  it('shows undetermined letters as preedit without committing', () => {
    const output = { consumed: true, delete_before: 0, commit: '', preedit: 'tato', focus: null };
    expect(applyOutput(empty, output, { type: 'char', value: 'o' })).toEqual({
      before: '',
      preedit: 'tato',
      after: ''
    });
  });

  it('commits text and then types the key that was not consumed', () => {
    // "kore" を英語として確定し、空白はそのまま入力する。
    const text: PadText = { before: 'a ', preedit: 'kore', after: 'z' };
    const output = { consumed: false, delete_before: 0, commit: 'kore', preedit: '', focus: null };
    expect(applyOutput(text, output, { type: 'space' })).toEqual({
      before: 'a kore ',
      preedit: '',
      after: 'z'
    });
  });

  it('types the key itself when the engine passes it through', () => {
    const passed = { consumed: false, delete_before: 0, commit: '', preedit: '', focus: null };
    const text: PadText = { before: 'ab', preedit: '', after: '' };
    expect(applyOutput(text, passed, { type: 'char', value: 'c' }).before).toBe('abc');
    expect(applyOutput(text, passed, { type: 'enter' }).before).toBe('ab\n');
    expect(applyOutput(text, passed, { type: 'backspace' }).before).toBe('a');
    expect(applyOutput(text, passed, { type: 'escape' }).before).toBe('ab');
  });

  it('deletes committed characters before rereading a word', () => {
    // "made " の 5 文字を消して、「まで」を未確定文字列にする。
    const text: PadText = { before: 'これはmade ', preedit: '', after: '' };
    const output = { consumed: true, delete_before: 5, commit: '', preedit: 'まで', focus: null };
    expect(applyOutput(text, output, { type: 'force_japanese' })).toEqual({
      before: 'これは',
      preedit: 'まで',
      after: ''
    });
  });

  it('counts surrogate pairs as one character', () => {
    const text: PadText = { before: 'a𠮷', preedit: '', after: '' };
    const passed = { consumed: false, delete_before: 0, commit: '', preedit: '', focus: null };
    expect(applyOutput(text, passed, { type: 'backspace' }).before).toBe('a');
  });

  it('applies a timeout commit without a key', () => {
    const text: PadText = { before: '', preedit: 'kore', after: '' };
    const output = { consumed: true, delete_before: 0, commit: 'kore', preedit: '', focus: null };
    expect(applyOutput(text, output, null)).toEqual({ before: 'kore', preedit: '', after: '' });
  });
});

describe('preeditSelection', () => {
  it('covers the whole preedit while composing', () => {
    expect(preeditSelection({ before: 'ab', preedit: 'たとえ', after: '' }, null)).toEqual([2, 5]);
  });

  it('covers only the focused segment while converting', () => {
    // 「今日は晴れ」のうち、3 文字目からの 2 文字。
    const text: PadText = { before: 'x', preedit: '今日は晴れ', after: '' };
    expect(preeditSelection(text, [3, 2])).toEqual([4, 6]);
  });

  it('counts positions in UTF-16 units for the text area', () => {
    // 「𠮷」は UTF-16 で 2 単位。
    const text: PadText = { before: '', preedit: '𠮷野は', after: '' };
    expect(preeditSelection(text, [1, 2])).toEqual([2, 4]);
  });
});

describe('commitPreedit', () => {
  it('keeps the preedit as committed text', () => {
    expect(commitPreedit({ before: 'a', preedit: 'たとえ', after: 'b' })).toEqual({
      before: 'aたとえ',
      preedit: '',
      after: 'b'
    });
  });
});
