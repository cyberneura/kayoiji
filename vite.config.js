import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import tailwindcss from "@tailwindcss/vite";

// @ts-expect-error process は Node.js のグローバル変数
const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  plugins: [tailwindcss(), sveltekit()],

  // 画面を消すと、Rust のビルドエラーが読めなくなる。
  clearScreen: false,
  // Tauri は決まったポートに接続するので、使えない時は別のポートに逃げずに失敗させる。
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // Rust のソース・ビルドの出力・辞書のデータが変わっても、画面を読み込み直さない。
      // 読み込み直すと、KayoijiPad で入力している途中の状態が消える。
      ignored: ["**/src-tauri/**", "**/crates/**", "**/target/**", "**/data/**", "**/_issues/**", "**/build/**"],
    },
  },
}));
