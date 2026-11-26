const JST_OFFSET_SECONDS = 9 * 60 * 60;

/**
 * UNIX 時間 (秒) を、`2026-11-26 15:10:07 (JST)` の形にする。
 *
 * 表示する端末のタイムゾーンに関係なく、日本時間で表す。日本時間には夏時間が無いので、
 * 時差を足した時刻を UTC として書き出せば足りる。
 */
export function formatJst(seconds: number): string {
  const shifted = new Date((seconds + JST_OFFSET_SECONDS) * 1000).toISOString();
  return `${shifted.slice(0, 10)} ${shifted.slice(11, 19)} (JST)`;
}
