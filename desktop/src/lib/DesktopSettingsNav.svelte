<script lang="ts">
  import { onMount, tick } from "svelte";
  import type { SettingsNavCategory } from "./desktopSettingsIndex.js";

  interface Props {
    categories: SettingsNavCategory[];
    activeId: string;
    query: string;
    /** Matching cards while searching, or null with no query. */
    matchCount: number | null;
    onselect: (id: string) => void;
    onsubmit: () => void;
  }

  let { categories, activeId, query = $bindable(), matchCount, onselect, onsubmit }: Props = $props();

  let searchInput: HTMLInputElement | undefined = $state();
  let list: HTMLUListElement | undefined = $state();

  // In a narrow window the list is a horizontal chip row: keep the active
  // chip in view without scrolling the page.
  $effect(() => {
    const id = activeId;
    if (!list) return;
    void tick().then(() => {
      if (!list || list.scrollWidth <= list.clientWidth) return;
      const chip = list.querySelector<HTMLElement>(`[data-category="${CSS.escape(id)}"]`);
      if (!chip) return;
      const start = chip.offsetLeft - list.offsetLeft;
      const end = start + chip.offsetWidth;
      if (start < list.scrollLeft || end > list.scrollLeft + list.clientWidth) {
        list.scrollTo({ left: Math.max(0, start - 16), behavior: "smooth" });
      }
    });
  });

  function handleSearchKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && query) {
      event.preventDefault();
      event.stopPropagation();
      query = "";
    } else if (event.key === "Enter") {
      event.preventDefault();
      onsubmit();
    }
  }

  function handleCategoryClick(event: MouseEvent, id: string) {
    event.preventDefault();
    onselect(id);
  }

  onMount(() => {
    // `/` focuses the search unless the user is typing somewhere else.
    const handleKeydown = (event: KeyboardEvent) => {
      if (event.key !== "/" || event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName) || target.isContentEditable)
      ) {
        return;
      }
      event.preventDefault();
      searchInput?.focus();
      searchInput?.select();
    };
    window.addEventListener("keydown", handleKeydown);
    return () => window.removeEventListener("keydown", handleKeydown);
  });
</script>

<nav class="settings-nav" aria-label="Settings categories">
  <label class="settings-nav-search">
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
      <circle cx="7" cy="7" r="4.5" fill="none" stroke="currentColor" stroke-width="1.6" />
      <path d="M10.5 10.5 14 14" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
    </svg>
    <input
      bind:this={searchInput}
      bind:value={query}
      type="search"
      placeholder="Search settings"
      aria-label="Search settings"
      autocomplete="off"
      spellcheck="false"
      onkeydown={handleSearchKeydown}
    />
    {#if !query}<kbd aria-hidden="true">/</kbd>{/if}
  </label>
  {#if matchCount !== null}
    <p class="settings-nav-summary" aria-live="polite">
      {matchCount === 1 ? "1 matching setting" : `${matchCount} matching settings`}
    </p>
  {/if}
  <ul class="settings-nav-list" bind:this={list}>
    {#each categories as category (category.id)}
      <li>
        <a
          href={`#${category.id}`}
          data-category={category.id}
          class:active={category.id === activeId}
          class:empty={category.count === 0}
          aria-current={category.id === activeId ? "location" : undefined}
          aria-disabled={category.count === 0 ? "true" : undefined}
          tabindex={category.count === 0 ? -1 : undefined}
          onclick={(event) => handleCategoryClick(event, category.id)}
        >
          <span>{category.label}</span>
          <span class="settings-nav-count">{category.count}</span>
        </a>
      </li>
    {/each}
  </ul>
</nav>

<style>
  .settings-nav {
    align-self: start;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: var(--radius, 8px);
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: calc(100vh - 32px);
    overflow-y: auto;
    padding: 12px 10px;
    position: sticky;
    top: 16px;
  }

  .settings-nav-search {
    align-items: center;
    background: color-mix(in srgb, var(--surface) 70%, var(--bg-card));
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-muted);
    display: flex;
    gap: 7px;
    min-width: 0;
    padding: 0 8px;
  }

  .settings-nav-search:focus-within {
    border-color: color-mix(in srgb, var(--accent) 70%, var(--border));
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 20%, transparent);
  }

  .settings-nav-search svg {
    flex: none;
  }

  .settings-nav-search input {
    background: transparent;
    border: 0;
    color: var(--text);
    flex: 1;
    font: inherit;
    font-size: 13px;
    min-width: 0;
    outline: none;
    padding: 7px 0;
  }

  .settings-nav-search input::placeholder {
    color: var(--text-muted);
  }

  kbd {
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--text-muted);
    font-family: var(--font-mono-local, ui-monospace, monospace);
    font-size: 11px;
    line-height: 1;
    padding: 2px 5px;
  }

  .settings-nav-summary {
    color: var(--text-muted);
    font-size: 12px;
    padding: 0 4px;
  }

  .settings-nav-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    list-style: none;
  }

  .settings-nav-list a {
    align-items: center;
    border: 1px solid transparent;
    border-radius: 6px;
    color: var(--text-muted);
    display: flex;
    font-size: 13px;
    font-weight: 600;
    gap: 8px;
    justify-content: space-between;
    padding: 7px 9px;
    text-decoration: none;
  }

  .settings-nav-list a:hover {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: var(--text);
  }

  .settings-nav-list a:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--accent) 70%, transparent);
    outline-offset: 1px;
  }

  .settings-nav-list a.active {
    background: color-mix(in srgb, var(--accent) 22%, var(--bg-card));
    border-color: color-mix(in srgb, var(--accent) 40%, var(--border));
    color: var(--text);
  }

  .settings-nav-list a.empty {
    opacity: 0.45;
    pointer-events: none;
  }

  .settings-nav-count {
    color: var(--text-muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    font-weight: 700;
  }

  /* Narrow window: a sticky bar with the search and a horizontally scrolling
     chip row. */
  @media (max-width: 819px) {
    .settings-nav {
      background: color-mix(in srgb, var(--bg) 94%, transparent);
      -webkit-backdrop-filter: blur(12px);
      backdrop-filter: blur(12px);
      border-radius: 0;
      border-width: 0 0 1px;
      gap: 8px;
      margin: 0 -16px;
      max-height: none;
      overflow: visible;
      padding: 8px 16px;
      top: 0;
      z-index: 25;
    }

    kbd,
    .settings-nav-summary {
      display: none;
    }

    .settings-nav-list {
      flex-direction: row;
      gap: 6px;
      margin: 0 -16px;
      overflow-x: auto;
      overscroll-behavior-x: contain;
      padding: 0 16px 2px;
      scrollbar-width: none;
    }

    .settings-nav-list::-webkit-scrollbar {
      display: none;
    }

    .settings-nav-list li {
      flex: none;
    }

    .settings-nav-list a {
      border-color: var(--border);
      border-radius: 999px;
      font-size: 12px;
      gap: 6px;
      padding: 5px 11px;
      white-space: nowrap;
    }
  }
</style>
