export type Mode = 'direct' | 'composing' | 'converting';

/** `auto` は英語か日本語かを判定しながら入力する。`direct` はすべての打鍵をそのまま通す。 */
export type InputMode = 'auto' | 'direct';

/** 候補ウインドウに表示する内容。Rust 側の `kayoiji_core::engine::View` と同じ形。 */
export interface View {
  input_mode: InputMode;
  /** 次の入力を、判定せずに日本語として始めるかどうか。 */
  forced: boolean;
  mode: Mode;
  reading: string;
  candidates: string[];
  selected: number | null;
  page: number;
  page_count: number;
}

export interface Settings {
  sequence_timeout_ms: number;
}

export interface AppStatus {
  settings: Settings;
  /** OS の入力メソッドとして動いているかどうか。 */
  input_method_running: boolean;
  version: string;
  rules_path: string;
  /** 起動時に読めなかった利用者のファイルと、その理由。 */
  warnings: string[];
}
