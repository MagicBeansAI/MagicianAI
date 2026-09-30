/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{html,js,svelte,ts}'],
  theme: {
    extend: {
      // Multi-theme color system (CSS variables are set per-theme)
      colors: {
        // Theme-adaptive colors via CSS variables
        'theme-bg': 'var(--bg-base)',
        'theme-surface': 'var(--bg-surface)',
        'theme-elevated': 'var(--bg-elevated)',
        'theme-text': 'var(--text-primary)',
        'theme-text-secondary': 'var(--text-secondary)',
        'theme-text-muted': 'var(--text-muted)',
        'theme-accent': 'var(--accent-primary)',
        'theme-accent-secondary': 'var(--accent-secondary)',
        'theme-border': 'var(--border-default)',

        // Legacy Soft Machine colors (for backwards compatibility)
        cream: '#fefdfb',
        warm: '#f8f6f2',
        soft: '#f3f0ea',
        ink: '#2d2a26',
        body: '#4a4540',
        muted: '#8a847a',
        faint: '#c4beb4',
        coral: {
          DEFAULT: '#e85d5d',
          bright: '#ff6b6b',
          soft: '#fff0f0',
          hover: '#d04f4f',
        },
        sage: {
          DEFAULT: '#6b9080',
          soft: '#e8f0ec',
        },
        plum: {
          DEFAULT: '#8b7ec8',
          soft: '#f0edf8',
        },
        success: {
          DEFAULT: '#5fa67a',
          soft: '#e8f5ed',
        },
        warning: {
          DEFAULT: '#d4a34a',
          soft: '#fdf6e8',
        },
        error: {
          DEFAULT: '#d4574a',
          soft: '#fdecea',
        },
        'border-soft': '#ebe7e0',
        'border-default': '#ddd8cf',
      },

      // Theme-adaptive font family
      fontFamily: {
        sans: ['var(--font-primary)', '-apple-system', 'BlinkMacSystemFont', 'sans-serif'],
        display: ['var(--font-display)', 'Georgia', 'serif'],
        mono: ['var(--font-mono)', 'JetBrains Mono', 'monospace'],
      },

      // Theme-adaptive border radius
      borderRadius: {
        'sm': 'var(--radius-sm, 8px)',
        'md': 'var(--radius-md, 12px)',
        'lg': 'var(--radius-lg, 20px)',
        'xl': 'var(--radius-xl, 28px)',
      },

      // Theme-adaptive shadows
      boxShadow: {
        'soft-sm': 'var(--shadow-sm)',
        'soft-md': 'var(--shadow-md)',
        'soft-lg': 'var(--shadow-lg)',
        'glow': 'var(--shadow-glow)',
      },
    },
  },
  plugins: [require('daisyui')],

  // DaisyUI config - primary shipped themes
  daisyui: {
    themes: [
      // ========== MAGICAN (Default Light) ==========
      {
        'magican': {
          "primary": "#ff6b6b",
          "primary-content": "#ffffff",
          "secondary": "#4ecdc4",
          "secondary-content": "#2d3436",
          "accent": "#ffe66d",
          "accent-content": "#2d3436",
          "neutral": "#2d3436",
          "neutral-content": "#ffffff",
          "base-100": "#fdfcf8",
          "base-200": "#fff8f2",
          "base-300": "#f6f1e8",
          "base-content": "#2d3436",
          "info": "#4ecdc4",
          "info-content": "#2d3436",
          "success": "#00bb7f",
          "success-content": "#ffffff",
          "warning": "#ffe66d",
          "warning-content": "#2d3436",
          "error": "#ff6b6b",
          "error-content": "#ffffff",
        },
      },
      // ========== MAGICAN DARK ==========
      {
        'magican-dark': {
          "primary": "#ff7b7b",
          "primary-content": "#ffffff",
          "secondary": "#54d3cb",
          "secondary-content": "#171b1d",
          "accent": "#ffd86a",
          "accent-content": "#171b1d",
          "neutral": "#242b2f",
          "neutral-content": "#f7f1e8",
          "base-100": "#171b1d",
          "base-200": "#1d2326",
          "base-300": "#242b2f",
          "base-content": "#f7f1e8",
          "info": "#54d3cb",
          "info-content": "#171b1d",
          "success": "#3cd39b",
          "success-content": "#171b1d",
          "warning": "#ffd86a",
          "warning-content": "#171b1d",
          "error": "#ff7b7b",
          "error-content": "#ffffff",
        },
      },
      // ========== SOFT MACHINE (Warm, Approachable) ==========
      {
        'soft-machine': {
          "primary": "#e85d5d",           // coral
          "primary-content": "#ffffff",
          "secondary": "#6b9080",          // sage
          "secondary-content": "#ffffff",
          "accent": "#8b7ec8",             // plum
          "accent-content": "#ffffff",
          "neutral": "#2d2a26",            // ink
          "neutral-content": "#fefdfb",
          "base-100": "#fefdfb",           // cream
          "base-200": "#f8f6f2",           // warm
          "base-300": "#f3f0ea",           // soft
          "base-content": "#2d2a26",       // ink
          "info": "#8b7ec8",               // plum
          "info-content": "#ffffff",
          "success": "#5fa67a",
          "success-content": "#ffffff",
          "warning": "#d4a34a",
          "warning-content": "#ffffff",
          "error": "#d4574a",
          "error-content": "#ffffff",
        },
      },
      // ========== ARCANE TERMINAL (Dark, Mystical Tech) ==========
      {
        'arcane-terminal': {
          "primary": "#00d4aa",           // neon cyan
          "primary-content": "#0a0a0f",
          "secondary": "#9d4edd",          // mystic purple
          "secondary-content": "#ffffff",
          "accent": "#ff6b35",             // ember orange
          "accent-content": "#0a0a0f",
          "neutral": "#1a1a2e",            // dark void
          "neutral-content": "#e0e0e0",
          "base-100": "#0a0a0f",           // deepest black
          "base-200": "#12121a",           // dark surface
          "base-300": "#1a1a2e",           // elevated surface
          "base-content": "#e0e0e0",       // light text
          "info": "#00d4aa",               // cyan
          "info-content": "#0a0a0f",
          "success": "#00ff9f",            // bright green
          "success-content": "#0a0a0f",
          "warning": "#ffb84d",            // gold
          "warning-content": "#0a0a0f",
          "error": "#ff4757",              // red alert
          "error-content": "#ffffff",
        },
      },
      // ========== EDITORIAL PRECISION (Refined Swiss Design) ==========
      {
        'editorial-precision': {
          "primary": "#1a1a1a",           // near black
          "primary-content": "#ffffff",
          "secondary": "#c9a227",          // gold accent
          "secondary-content": "#1a1a1a",
          "accent": "#8b4513",             // warm brown
          "accent-content": "#ffffff",
          "neutral": "#333333",            // dark gray
          "neutral-content": "#f5f5f5",
          "base-100": "#fafaf8",           // warm white
          "base-200": "#f0f0ed",           // light gray
          "base-300": "#e5e5e0",           // medium gray
          "base-content": "#1a1a1a",       // near black text
          "info": "#4a90a4",               // muted blue
          "info-content": "#ffffff",
          "success": "#2d5a27",            // forest green
          "success-content": "#ffffff",
          "warning": "#c9a227",            // gold
          "warning-content": "#1a1a1a",
          "error": "#8b2500",              // deep red
          "error-content": "#ffffff",
        },
      },
    ],
  },
}
