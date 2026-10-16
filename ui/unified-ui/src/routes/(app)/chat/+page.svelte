<!--
  /chat — the main chat surface.

  Post-relocation, the entire chat experience lives in the reusable
  `<ChatPanel/>` component at `$lib/magician/chat/ChatPanel.svelte`.
  Every feature, store subscription, helper function, event handler,
  message-type branch, modal, and CSS rule that used to be inline in
  this 5000-line page now lives there, where /hud (and any future
  surface) can mount it.

  This file is a one-liner so the route still exists for SvelteKit's
  router. Adding chrome around the chat goes here — wrap <ChatPanel/>
  with whatever sidebar / workbench / breadcrumb / banner this surface
  needs. The default (no wrapper) gives you the full chat as it
  always has.
-->
<script lang="ts">
	import ChatPanel from '$lib/magician/chat/ChatPanel.svelte';
</script>

<ChatPanel />
