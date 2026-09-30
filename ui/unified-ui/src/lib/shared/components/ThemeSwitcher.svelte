<script lang="ts">
  import { onMount } from 'svelte';
  import { browser } from '$app/environment';

  import { themeStore, type Theme } from '$lib/shared/stores/themeStore';

  export let minimal = false;
  export let iconOnly = false;

  interface ThemeInfo {
    id: Theme;
    name: string;
    description: string;
    icon: string;
    isDark: boolean;
    preview: {
      bg: string;
      accent: string;
      text: string;
    };
  }

  const themes: ThemeInfo[] = [
    // Longhand — magican's house personality. Default theme; pinned to the top.
    {
      id: 'longhand',
      name: 'Longhand',
      description: 'Hand-printed cream & ink',
      icon: '✎',
      isDark: false,
      preview: { bg: '#f3ead6', accent: '#a04020', text: '#1a1612' }
    },
    {
      id: 'longhand-dark',
      name: 'Longhand Dark',
      description: 'Warm graphite paper, cream ink',
      icon: '✎',
      isDark: true,
      preview: { bg: '#1a1612', accent: '#d8602e', text: '#f3ead6' }
    },
    // Retro themes
    {
      id: 'retro-16bit',
      name: 'Retro Dark',
      description: 'Amber phosphor on warm black',
      icon: '📺',
      isDark: true,
      preview: { bg: '#0c0a08', accent: '#ffb000', text: '#ffb000' }
    },
    {
      id: 'retro-16bit-light',
      name: 'Retro Light',
      description: 'Paper ASCII',
      icon: '📠',
      isDark: false,
      preview: { bg: '#f5f5f0', accent: '#1a1a1a', text: '#1a1a1a' }
    },
    // Mario 8-bit — NES Super Mario Bros palette
    {
      id: 'mario-8bit',
      name: 'Mario 8-bit',
      description: 'NES sky overworld',
      icon: '🍄',
      isDark: false,
      preview: { bg: '#5c94fc', accent: '#e40000', text: '#000000' }
    },
    {
      id: 'mario-8bit-dark',
      name: 'Mario Underground',
      description: 'Castle dungeon, coin glow',
      icon: '⭐',
      isDark: true,
      preview: { bg: '#000000', accent: '#fbd000', text: '#ffffff' }
    },
    // Risograph — fluorescent spot-color print
    {
      id: 'risograph',
      name: 'Risograph',
      description: 'Indie spot-color print, fluorescent pink',
      icon: '🟣',
      isDark: false,
      preview: { bg: '#f5efe1', accent: '#ff48b0', text: '#1a1612' }
    },
    {
      id: 'risograph-dark',
      name: 'Risograph Dark',
      description: 'Fluo pink on warm graphite',
      icon: '🟪',
      isDark: true,
      preview: { bg: '#14110d', accent: '#ff48b0', text: '#f4efe1' }
    },
    // Mixtape — hand-labelled cassette tape
    {
      id: 'mixtape',
      name: 'Mixtape',
      description: 'Mustard label, sharpie marker',
      icon: '📼',
      isDark: false,
      preview: { bg: '#e8c66a', accent: '#c8362a', text: '#2a1a0e' }
    },
    {
      id: 'mixtape-dark',
      name: 'Mixtape (Side B)',
      description: 'Cassette body, tape oxide',
      icon: '🎵',
      isDark: true,
      preview: { bg: '#0f0e0c', accent: '#ff5040', text: '#e8c66a' }
    },
    // Monochrome — pure grayscale, no chromatic accent
    {
      id: 'mono',
      name: 'Mono',
      description: 'Pure grayscale, black on white',
      icon: '○',
      isDark: false,
      preview: { bg: '#ffffff', accent: '#000000', text: '#000000' }
    },
    {
      id: 'mono-dark',
      name: 'Mono Dark',
      description: 'Pure grayscale, white on black',
      icon: '●',
      isDark: true,
      preview: { bg: '#000000', accent: '#ffffff', text: '#ffffff' }
    },
    // 2D Cartoon — flat pastel + black-outline
    {
      id: 'cartoon',
      name: '2D Cartoon',
      description: 'Sky-blue clouds, sticker outlines',
      icon: '🎨',
      isDark: false,
      preview: { bg: '#9dd5ff', accent: '#ff5499', text: '#0a0a08' }
    },
    {
      id: 'cartoon-dark',
      name: '2D Cartoon Night',
      description: 'Starry purple sky, cream cel-ink',
      icon: '🌙',
      isDark: true,
      preview: { bg: '#1a1830', accent: '#ff7eb6', text: '#fff5e1' }
    },
    // Bubbly — Magican About page palette as a reusable app theme
    {
      id: 'bubbly',
      name: 'Bubbly',
      description: 'Cream canvas, coral bubbles, teal float',
      icon: '◌',
      isDark: false,
      preview: { bg: '#fdfcf8', accent: '#ff6b6b', text: '#2d3436' }
    },
    {
      id: 'bubbly-dark',
      name: 'Bubbly Dark',
      description: 'Midnight cream ink with coral + teal glow',
      icon: '●',
      isDark: true,
      preview: { bg: '#171b1d', accent: '#ff7b7b', text: '#f7f1e8' }
    },
    // Light themes
    {
      id: 'soft-machine',
      name: 'Soft Machine',
      description: 'Warm & Approachable',
      icon: '🌸',
      isDark: false,
      preview: { bg: '#fefdfb', accent: '#e85d5d', text: '#2d2a26' }
    },
    {
      id: 'soft-machine-dark',
      name: 'Soft Machine Dark',
      description: 'Warm after dark',
      icon: '◐',
      isDark: true,
      preview: { bg: '#171b1d', accent: '#ff7b7b', text: '#f7f1e8' }
    },
    {
      id: 'arcane-terminal-light',
      name: 'Arcane Terminal Light',
      description: 'Day-mode developer notebook',
      icon: '🔮',
      isDark: false,
      preview: { bg: '#f6f8fa', accent: '#007a66', text: '#0a0a0f' }
    },
    // Dark themes
    {
      id: 'arcane-terminal',
      name: 'Arcane Terminal',
      description: 'Dark & Mystical',
      icon: '🔮',
      isDark: true,
      preview: { bg: '#0a0a0f', accent: '#00d4aa', text: '#e0e0e0' }
    },
    // Jarvis — Iron Man HUD aesthetic; cyan + green over deep navy.
    // Companion `jarvis-light` for high-key environments.
    {
      id: 'jarvis',
      name: 'Jarvis',
      description: 'Iron Man HUD — glass + cyan glow',
      icon: '◈',
      isDark: true,
      preview: { bg: '#050a14', accent: '#00d4ff', text: '#d8ecff' }
    },
    {
      id: 'jarvis-light',
      name: 'Jarvis Light',
      description: 'HUD aesthetic on icy white',
      icon: '◇',
      isDark: false,
      preview: { bg: '#f0f7ff', accent: '#0099cc', text: '#062035' }
    }
  ];

  let currentTheme: Theme = 'longhand';
  let isOpen = false;

  function setTheme(theme: Theme) {
    themeStore.setTheme(theme);
    isOpen = false;
  }

  function toggleDropdown() {
    isOpen = !isOpen;
  }

  function handleClickOutside(event: MouseEvent) {
    const target = event.target as HTMLElement;
    if (!target.closest('.theme-switcher')) {
      isOpen = false;
    }
  }

  onMount(() => {
    themeStore.init();
    const unsubscribe = themeStore.subscribe(value => {
      currentTheme = value;
    });

    if (browser) {
      document.addEventListener('click', handleClickOutside);
    }
    
    return () => {
      unsubscribe();
      if (browser) {
        document.removeEventListener('click', handleClickOutside);
      }
    };
  });

  $: currentThemeInfo = themes.find(t => t.id === currentTheme) || themes[0];
</script>

<div class="theme-switcher" class:minimal class:icon-only={iconOnly}>
  {#if iconOnly}
    <button
      class="theme-icon-btn"
      on:click={toggleDropdown}
      aria-expanded={isOpen}
      aria-haspopup="listbox"
      aria-label="Change theme"
      title="Change theme"
    >
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="13.5" cy="6.5" r="1.5" fill="currentColor"/>
        <circle cx="17.5" cy="10.5" r="1.5" fill="currentColor"/>
        <circle cx="8.5" cy="7.5" r="1.5" fill="currentColor"/>
        <circle cx="6.5" cy="12.5" r="1.5" fill="currentColor"/>
        <path d="M12 2C6.49 2 2 6.49 2 12s4.49 10 10 10c.83 0 1.5-.67 1.5-1.5 0-.39-.15-.74-.39-1.01-.23-.26-.38-.61-.38-.99 0-.83.67-1.5 1.5-1.5H16c3.31 0 6-2.69 6-6 0-4.96-4.49-9-10-9z"/>
      </svg>
    </button>
  {:else}
    <button
      class="theme-toggle"
      class:minimal-btn={minimal}
      on:click={toggleDropdown}
      aria-expanded={isOpen}
      aria-haspopup="listbox"
      title="Change theme"
    >
      <span class="theme-icon">{currentThemeInfo.icon}</span>
      <span class="theme-label">{minimal ? 'Theme' : currentThemeInfo.name}</span>
      <svg class="chevron" class:open={isOpen} width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <polyline points="6 9 12 15 18 9"></polyline>
      </svg>
    </button>
  {/if}

  {#if isOpen}
    <div class="theme-dropdown" class:dropdown-up={minimal && !iconOnly} role="listbox">
      <!-- Light Themes Section -->
      <div class="section-header">Light</div>
      {#each themes.filter(t => !t.isDark) as theme}
        <button
          class="theme-option"
          class:active={currentTheme === theme.id}
          on:click={() => setTheme(theme.id)}
          role="option"
          aria-selected={currentTheme === theme.id}
        >
          <div class="theme-preview" style="background: {theme.preview.bg}; border-color: {theme.preview.accent};">
            <span class="preview-accent" style="background: {theme.preview.accent};"></span>
          </div>
          <div class="theme-info">
            <span class="theme-name">{theme.name}</span>
            <span class="theme-desc">{theme.description}</span>
          </div>
          {#if currentTheme === theme.id}
            <svg class="check" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3">
              <polyline points="20 6 9 17 4 12"></polyline>
            </svg>
          {/if}
        </button>
      {/each}

      <!-- Dark Themes Section -->
      <div class="section-header">Dark</div>
      {#each themes.filter(t => t.isDark) as theme}
        <button
          class="theme-option"
          class:active={currentTheme === theme.id}
          on:click={() => setTheme(theme.id)}
          role="option"
          aria-selected={currentTheme === theme.id}
        >
          <div class="theme-preview" style="background: {theme.preview.bg}; border-color: {theme.preview.accent};">
            <span class="preview-accent" style="background: {theme.preview.accent};"></span>
          </div>
          <div class="theme-info">
            <span class="theme-name">{theme.name}</span>
            <span class="theme-desc">{theme.description}</span>
          </div>
          {#if currentTheme === theme.id}
            <svg class="check" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3">
              <polyline points="20 6 9 17 4 12"></polyline>
            </svg>
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .theme-switcher {
    position: relative;
    z-index: 1000;
  }

  /* Icon-only variant: 32×32 button matching the topbar icon-btn size, with
     a downward+right-anchored dropdown bounded by viewport + attention bar. */
  .theme-icon-btn {
    width: 32px;
    height: 32px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 8px;
    color: var(--text-secondary, #555);
    cursor: pointer;
    transition: background 120ms ease, color 120ms ease, border-color 120ms ease;
  }

  .theme-icon-btn:hover {
    background: var(--bg-soft, rgba(0, 0, 0, 0.05));
    color: var(--text-primary, #1a1a1a);
    border-color: var(--border-soft, rgba(0, 0, 0, 0.08));
  }

  .icon-only .theme-dropdown {
    top: calc(100% + 8px);
    right: 0;
    left: auto;
    bottom: auto;
    max-height: calc(100vh - 64px - var(--attention-bar-offset, 0px) - 16px);
    max-width: calc(100vw - 32px);
  }

  .theme-toggle {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.6rem 1rem;
    background: var(--bg-card, rgba(255, 255, 255, 0.7));
    backdrop-filter: blur(12px);
    border: 1px solid var(--border-soft, rgba(235, 231, 224, 0.3));
    border-radius: 100px;
    color: var(--text-primary, #2d2a26);
    font-size: 0.85rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.3s cubic-bezier(0.4, 0, 0.2, 1);
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.05);
  }

  .theme-toggle:hover {
    transform: translateY(-2px);
    border-color: var(--accent-primary, #e85d5d);
    box-shadow: 0 8px 20px rgba(0, 0, 0, 0.08);
    background: var(--bg-elevated, #fff);
  }

  .theme-toggle.minimal-btn {
    min-width: 0;
    width: 100%;
    background: transparent;
    border: 1px solid transparent;
    box-shadow: none;
    padding: 0.5rem 0.75rem;
    border-radius: 8px;
    gap: 0.75rem;
    color: var(--text-secondary, #666);
    backdrop-filter: none;
  }

  .theme-toggle.minimal-btn:hover {
    background: var(--bg-surface, rgba(0, 0, 0, 0.05));
    color: var(--text-primary, #1a1a1a);
    transform: none;
  }

  .theme-icon {
    font-size: 1.1rem;
  }

  .theme-label {
    display: none;
  }

  .minimal .theme-label {
    display: inline;
    font-size: 0.875rem;
    font-weight: 500;
  }

  @media (min-width: 640px) {
    .theme-toggle:not(.minimal-btn) {
      min-width: 180px;
    }

    .theme-label {
      display: inline;
    }
  }

  .chevron {
    transition: transform 0.3s ease;
    opacity: 0.6;
    margin-left: auto;
  }

  .chevron.open {
    transform: rotate(180deg);
  }

  .theme-dropdown {
    position: absolute;
    top: calc(100% + 0.5rem);
    right: 0;
    min-width: 280px;
    max-height: 380px;
    overflow-y: auto;
    background: var(--bg-card, rgba(255, 255, 255, 0.8));
    backdrop-filter: blur(24px);
    border: 1px solid var(--border-soft, rgba(235, 231, 224, 0.4));
    border-radius: 20px;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.15);
    animation: dropdown-appear 0.3s cubic-bezier(0.16, 1, 0.3, 1);
    z-index: 1001;
    padding: 0.5rem;
  }

  .theme-dropdown.dropdown-up {
    bottom: calc(100% + 0.5rem);
    top: auto;
    left: 0;
    right: auto;
    transform-origin: bottom left;
  }

  .section-header {
    padding: 0.55rem 0.75rem 0.18rem;
    font-family: var(--font-display);
    font-size: 0.6rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    color: var(--text-muted, #888);
    opacity: 0.55;
  }

  @keyframes dropdown-appear {
    from {
      opacity: 0;
      transform: translateY(-10px) scale(0.95);
    }
    to {
      opacity: 1;
      transform: translateY(0) scale(1);
    }
  }

  .theme-option {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    width: 100%;
    padding: 0.45rem 0.6rem;
    background: transparent;
    border: none;
    cursor: pointer;
    text-align: left;
    transition: background 0.2s ease, transform 0.2s ease;
    border-radius: 8px;
    margin-bottom: 1px;
  }

  .theme-option:hover {
    background: var(--bg-soft, rgba(0, 0, 0, 0.03));
    transform: translateX(2px);
  }

  .theme-option.active {
    background: var(--accent-primary-soft, rgba(232, 93, 93, 0.1));
  }

  /* Preview square — slimmer, 22×22, 6px radius. */
  .theme-preview {
    width: 22px;
    height: 22px;
    border-radius: 6px;
    border: 1.5px solid;
    display: flex;
    align-items: flex-end;
    justify-content: flex-end;
    padding: 2px;
    flex-shrink: 0;
    box-shadow: 0 2px 4px rgba(0, 0, 0, 0.04);
  }

  .preview-accent {
    width: 8px;
    height: 8px;
    border-radius: 2px;
  }

  .theme-info {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
    min-width: 0;
  }

  .theme-name {
    font-family: var(--font-display);
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--text-primary, #2d2a26);
    line-height: 1.2;
  }

  .theme-desc {
    font-family: var(--font-primary);
    font-size: 0.68rem;
    font-weight: 400;
    color: var(--text-muted, #888);
    opacity: 0.85;
    line-height: 1.3;
  }

  .check {
    color: var(--accent-primary, #e85d5d);
    flex-shrink: 0;
    background: var(--bg-elevated, #fff);
    border-radius: 50%;
    padding: 2px;
    box-shadow: 0 2px 6px rgba(0, 0, 0, 0.1);
  }

  /* Dark Theme Dropdown Overrides */
  :global([data-theme="arcane-terminal"]) .theme-option:hover {
    background: rgba(255, 255, 255, 0.05);
  }

  /* Retro 16-bit Theme Overrides */
  :global([data-theme^="retro-16bit"]) .theme-toggle {
    border-radius: 0 !important;
    border: 1px solid var(--text-primary) !important;
    background: var(--bg-base) !important;
    font-family: var(--font-mono) !important;
    text-transform: uppercase !important;
    box-shadow: none !important;
  }

  :global([data-theme^="retro-16bit"]) .theme-dropdown {
    border-radius: 0 !important;
    border: 2px solid var(--text-primary) !important;
    background: var(--bg-base) !important;
    box-shadow: 8px 8px 0 var(--text-muted) !important;
    backdrop-filter: none !important;
  }

  :global([data-theme^="retro-16bit"]) .theme-option {
    border-radius: 0 !important;
    font-family: var(--font-mono) !important;
    text-transform: uppercase !important;
  }

  :global([data-theme^="retro-16bit"]) .theme-option:hover,
  :global([data-theme^="retro-16bit"]) .theme-option.active:hover {
    background: var(--text-primary) !important;
    color: var(--bg-base) !important;
    transform: none !important;
  }

  :global([data-theme^="retro-16bit"]) .theme-option:hover .theme-name,
  :global([data-theme^="retro-16bit"]) .theme-option:hover .theme-desc,
  :global([data-theme^="retro-16bit"]) .theme-option.active:hover .theme-name,
  :global([data-theme^="retro-16bit"]) .theme-option.active:hover .theme-desc {
    color: var(--bg-base) !important;
  }

  :global([data-theme^="retro-16bit"]) .theme-option.active {
    background: var(--bg-soft) !important;
    border: 1px dashed var(--text-primary) !important;
  }

  :global([data-theme^="retro-16bit"]) .theme-name,
  :global([data-theme^="retro-16bit"]) .theme-desc,
  :global([data-theme^="retro-16bit"]) .section-header {
    font-family: var(--font-mono) !important;
    color: var(--text-primary) !important;
  }

  :global([data-theme^="retro-16bit"]) .theme-preview {
    border-radius: 0 !important;
  }

  /* Mario 8-bit Theme Overrides — same pixel-blocky pattern as retro,
     but NES-authentic chunky black drop-shadow. Dark variant gets a white
     drop-shadow (sprite outline against the underground black). */
  :global([data-theme^="mario-8bit"]) .theme-toggle,
  :global([data-theme^="mario-8bit"]) .theme-icon-btn {
    border-radius: 0 !important;
    border: 2px solid var(--border-default) !important;
    background: var(--bg-card) !important;
    font-family: var(--font-display) !important;
    font-size: 0.65rem !important;
    text-transform: uppercase !important;
    box-shadow: 3px 3px 0 var(--border-default) !important;
  }

  :global([data-theme^="mario-8bit"]) .theme-dropdown {
    border-radius: 0 !important;
    border: 2px solid var(--border-default) !important;
    background: var(--bg-card) !important;
    box-shadow: 4px 4px 0 var(--border-default) !important;
    backdrop-filter: none !important;
    padding: 0.6rem !important;
  }

  :global([data-theme^="mario-8bit"]) .theme-option {
    border-radius: 0 !important;
    font-family: var(--font-display) !important;
    text-transform: uppercase !important;
  }

  :global([data-theme^="mario-8bit"]) .theme-option:hover {
    background: var(--accent-primary) !important;
    transform: none !important;
  }

  :global([data-theme^="mario-8bit"]) .theme-option:hover .theme-name,
  :global([data-theme^="mario-8bit"]) .theme-option:hover .theme-desc {
    color: var(--text-on-accent) !important;
  }

  :global([data-theme^="mario-8bit"]) .theme-option.active {
    background: var(--accent-primary-soft) !important;
    border: 2px dashed var(--border-default) !important;
  }

  :global([data-theme^="mario-8bit"]) .theme-name,
  :global([data-theme^="mario-8bit"]) .theme-desc,
  :global([data-theme^="mario-8bit"]) .section-header {
    font-family: var(--font-display) !important;
    color: var(--text-primary) !important;
  }

  /* Press Start 2P at small sizes is dense — ease the desc text down so
     two lines fit cleanly inside the chunky 22×22 preview row. */
  :global([data-theme^="mario-8bit"]) .theme-name {
    font-size: 0.62rem !important;
    line-height: 1.4 !important;
  }

  :global([data-theme^="mario-8bit"]) .theme-desc {
    font-size: 0.5rem !important;
    line-height: 1.5 !important;
    color: var(--text-muted) !important;
  }

  :global([data-theme^="mario-8bit"]) .section-header {
    font-size: 0.55rem !important;
  }

  :global([data-theme^="mario-8bit"]) .theme-preview {
    border-radius: 0 !important;
    border-width: 2px !important;
  }
</style>
