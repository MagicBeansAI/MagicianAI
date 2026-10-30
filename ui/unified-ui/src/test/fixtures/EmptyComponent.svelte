<script lang="ts">
    // Accept props from unrelated page sections without mounting their network lifecycles.
    let { ..._props }: Record<string, unknown> = $props();
</script>
