<script lang="ts">
    import { onMount } from 'svelte';
    export let listening = false;
    export let active = false;
    export let inputAnalyser: AnalyserNode | null = null;
    export let outputAnalyser: AnalyserNode | null = null;
    let samples = Array<number>(25).fill(0);
    let timer: ReturnType<typeof setInterval>;
    const buffers = new WeakMap<AnalyserNode, Uint8Array<ArrayBuffer>>();

    function level(analyser: AnalyserNode | null): number {
        if (!analyser || analyser.context.state !== 'running') return 0;
        let bytes = buffers.get(analyser);
        if (!bytes) {
            bytes = new Uint8Array(new ArrayBuffer(analyser.fftSize));
            buffers.set(analyser, bytes);
        }
        analyser.getByteTimeDomainData(bytes);
        let sum = 0;
        for (const byte of bytes) sum += ((byte - 128) / 128) ** 2;
        const rms = Math.sqrt(sum / bytes.length);
        return rms > 0.00001 ? Math.max(0, Math.min(1, (20 * Math.log10(rms) + 50) / 50)) : 0;
    }

    onMount(() => {
        const motion = window.matchMedia('(prefers-reduced-motion: reduce)');
        timer = setInterval(() => {
            const value = active ? Math.max(level(outputAnalyser), listening ? level(inputAnalyser) : 0) : 0;
            samples = motion.matches ? Array<number>(25).fill(value) : [...samples.slice(1), value];
        }, 50);
        return () => clearInterval(timer);
    });
</script>

<div class="voice-waveform" aria-hidden="true">
    {#each samples as value, index (index)}
        <span style:height="{2 + value * 22}px"></span>
    {/each}
</div>

<style>
    .voice-waveform {
        display: flex;
        align-items: center;
        justify-content: space-between;
        width: 148px;
        max-width: 100%;
        height: 24px;
        margin: 4px auto;
        pointer-events: none;
        color: var(--accent-primary);
    }
    span {
        display: block;
        width: 3px;
        border-radius: 2px;
        background: linear-gradient(to bottom, color-mix(in srgb, currentColor 45%, transparent), currentColor, color-mix(in srgb, currentColor 45%, transparent));
        box-shadow: 0 0 6px color-mix(in srgb, currentColor 28%, transparent);
    }
</style>
