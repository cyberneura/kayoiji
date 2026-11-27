/** 絵文字の一覧で、一度に見せる行の数。候補ウインドウの高さを一定にする。 */
export const VISIBLE_ROWS = 6;

/**
 * 絵文字を `columns` 個ずつの行に分け、選んでいる絵文字が入る `visible` 行だけを返す。
 * 選んでいる行は、なるべく上から 3 行目に置く。`start` は返した最初の行の、一覧の中での位置。
 */
export function visibleRows(
  items: string[],
  columns: number,
  selected: number | null,
  visible = VISIBLE_ROWS
): { start: number; rows: string[][] } {
  const width = Math.max(1, columns);
  const rows: string[][] = [];
  for (let index = 0; index < items.length; index += width) {
    rows.push(items.slice(index, index + width));
  }
  const selectedRow = selected === null ? 0 : Math.floor(selected / width);
  const start = Math.max(0, Math.min(selectedRow - 2, rows.length - visible));
  return { start, rows: rows.slice(start, start + visible) };
}
