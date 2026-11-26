import { describe, expect, it } from 'vitest';
import { formatJst } from './time';

describe('formatJst', () => {
  it('writes the time in Japan with the zone name', () => {
    // Arrange: 2026-11-26 06:10:07 UTC
    const seconds = Date.UTC(2026, 10, 26, 6, 10, 7) / 1000;

    // Act
    const text = formatJst(seconds);

    // Assert
    expect(text).toBe('2026-11-26 15:10:07 (JST)');
  });

  it('moves to the next day when the time in Japan is past midnight', () => {
    // Arrange: 2026-12-31 15:00:00 UTC は、日本時間では翌年の 0 時。
    const seconds = Date.UTC(2026, 11, 31, 15, 0, 0) / 1000;

    // Act
    const text = formatJst(seconds);

    // Assert
    expect(text).toBe('2027-01-01 00:00:00 (JST)');
  });
});
