<script lang="ts">
  import { goto } from '$app/navigation';
  import { onMount } from 'svelte';

  // ウインドウは /overlay と /settings を直接開く。ここに来るのは開発時にルートを開いた時だけ。
  onMount(() => goto('/settings'));
</script>
