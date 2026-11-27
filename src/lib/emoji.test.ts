import { describe, expect, it } from 'vitest';
import { visibleRows } from './emoji';

const items = Array.from({ length: 25 }, (_, index) => String(index));

describe('visibleRows', () => {
  it('starts at the top while the selection is in the first rows', () => {
    // Act
    const shown = visibleRows(items, 4, 5, 3);

    // Assert
    expect(shown.start).toBe(0);
    expect(shown.rows).toEqual([
      ['0', '1', '2', '3'],
      ['4', '5', '6', '7'],
      ['8', '9', '10', '11']
    ]);
  });

  it('keeps the selected row third from the top', () => {
    // Act
    const shown = visibleRows(items, 4, 17, 3);

    // Assert: 17 は 5 行目 (位置 4)。その 2 行上から出す。
    expect(shown.start).toBe(2);
    expect(shown.rows[2]).toContain('17');
  });

  it('does not scroll past the last row', () => {
    // Act
    const shown = visibleRows(items, 4, 24, 3);

    // Assert
    expect(shown.start).toBe(4);
    expect(shown.rows).toEqual([['16', '17', '18', '19'], ['20', '21', '22', '23'], ['24']]);
  });

  it('shows everything when there are fewer rows than visible', () => {
    expect(visibleRows(['a', 'b'], 4, null, 3)).toEqual({ start: 0, rows: [['a', 'b']] });
  });
});
