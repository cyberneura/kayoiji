/**
 * `checking` は、英文チェックで英文を受け取っている状態。`View.reading` がその英文。
 * `emoji` は絵文字モード。`View.candidates` が一覧の絵文字、`View.emoji` がそのほかの内容。
 */
export type Mode = 'direct' | 'composing' | 'converting' | 'checking' | 'emoji';

/**
 * `japanese` はすべての打鍵を日本語として入力する。`auto` は英語か日本語かを判定しながら入力する。
 * `direct` は、入力モードを切り替えるキー以外の打鍵をそのまま通す。
 * `english` は英文チェック。英文を未確定文字列として受け取り、確定するまで入力先に出さない。
 */
export type InputMode = 'japanese' | 'auto' | 'direct' | 'english';

/** 綴りが誤っている単語と、その訂正候補。Rust 側の `kayoiji_check::spelling::Misspelling` と同じ形。 */
export interface Misspelling {
  word: string;
  /** 英文の先頭からの位置 (UTF-16 の単位)。 */
  start: number;
  guesses: string[];
}

/** 英文の綴りを調べた結果。 */
export interface Spelling {
  misspellings: Misspelling[];
  /** 綴りが誤っている単語を、最初の訂正候補に置き換えた文。置き換える所が無ければ null。 */
  corrected: string | null;
  /** 訂正候補をまだ作っていない単語があるかどうか。打鍵が止まると作られる。 */
  pending: boolean;
}

/** 言語モデルが作った訂正文と訳。Rust 側の `kayoiji_check::review::Review` と同じ形。 */
export interface Review {
  id: number;
  corrected: string;
  /** 訂正文が、頼んだ英文と実質的に同じかどうか。同じなら、英文に直す所は無い。 */
  unchanged: boolean;
  translation: string;
}

/**
 * Tab で英文と置き換える訂正案。言語モデルの訂正文があればそれ、無ければ綴りだけを直した文。
 * 直す所が無ければ null。
 */
export function suggestion(spelling: Spelling | null, review: Review | null): string | null {
  if (review) return review.unchanged ? null : review.corrected;
  return spelling?.corrected ?? null;
}

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
  emoji: EmojiView | null;
}

/** 絵文字モードの内容。Rust 側の `kayoiji_core::emoji_search::EmojiView` と同じ形。 */
export interface EmojiView {
  /** 打っている検索語 (かな)。 */
  query: string;
  groups: string[];
  /** 一覧を見ているグループ。検索している間は null。 */
  group: number | null;
  /** 一覧の 1 行に並べる数。 */
  columns: number;
  /** 選んでいる絵文字の名前。 */
  name: string;
  /** 肌の色の違いを出している時の、基本の絵文字と色違い。 */
  variants: string[];
  variant: number | null;
}

export interface Settings {
  sequence_timeout_ms: number;
  /** 英語か日本語かを判定しながら入力する。false なら、「かな」「英数」キーで切り替える。 */
  auto_switch: boolean;
  /** 日本語入力で英語を打ち始めたら (大文字、ローマ字として読めない並び)、直接入力に切り替える。 */
  leave_japanese_for_english: boolean;
}

export interface AppStatus {
  settings: Settings;
  /** OS の入力メソッドとして動いているかどうか。 */
  input_method_running: boolean;
  version: string;
  /** ビルドした時刻 (UNIX 時間の秒)。 */
  built_at: number;
  rules_path: string;
  /** 起動時に読めなかった利用者のファイルと、その理由。 */
  warnings: string[];
}
