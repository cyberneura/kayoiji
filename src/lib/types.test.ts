import { describe, expect, it } from 'vitest';
import { suggestion, type Review, type Spelling } from './types';

const spelling: Spelling = {
  misspellings: [{ word: 'fiel', start: 19, guesses: ['file'] }],
  corrected: 'Please send me the file',
  pending: false
};

function review(corrected: string, unchanged: boolean): Review {
  return { id: 1, corrected, unchanged, translation: '' };
}

describe('suggestion', () => {
  it('offers the spelling fix until the language model answers', () => {
    expect(suggestion(spelling, null)).toBe('Please send me the file');
  });

  it('prefers the correction of the language model', () => {
    // Arrange: 綴りは直せても、文法までは直らない
    const answer = review('Please send me the file.', false);

    // Act, Assert
    expect(suggestion(spelling, answer)).toBe('Please send me the file.');
  });

  it('offers nothing when the language model finds nothing to fix', () => {
    // 固有名詞のように、スペルチェッカーだけが誤りとみなした単語を置き換えない。
    expect(suggestion(spelling, review('Please send me the fiel', true))).toBeNull();
  });

  it('offers nothing for a text without problems', () => {
    expect(suggestion({ misspellings: [], corrected: null, pending: false }, null)).toBeNull();
    expect(suggestion(null, null)).toBeNull();
  });
});
