// Numerically mirrored from magios/Shared/AmbientOrbAppearance.swift.
// Phase meaning is shared across platforms; rendering technique is not.
export type RGB = readonly [number, number, number];

export interface AuroraPalette {
  coreA: RGB;
  coreB: RGB;
  accent: RGB;
  halo: RGB;
  haloStrength: number;
  rimStrength: number;
  seed: number;
}

export const AURORA_PALETTES = {
  armed_ember: {
    coreA: [0.36, 0.3, 0.52],
    coreB: [0.2, 0.17, 0.3],
    accent: [0.55, 0.42, 0.95],
    halo: [0.55, 0.42, 0.95],
    haloStrength: 0.16,
    rimStrength: 0.35,
    seed: 0.9,
  },
  violet_surge: {
    coreA: [0.62, 0.35, 1],
    coreB: [1, 0.42, 0.78],
    accent: [0.62, 0.35, 1],
    halo: [0.78, 0.45, 1],
    haloStrength: 0.9,
    rimStrength: 0.8,
    seed: 2.1,
  },
  calm_aurora: {
    coreA: [0.55, 0.38, 0.98],
    coreB: [0.36, 0.48, 1],
    accent: [0.6, 0.5, 1],
    halo: [0.6, 0.5, 1],
    haloStrength: 0.55,
    rimStrength: 0.6,
    seed: 3.3,
  },
  amber_thinking: {
    coreA: [1, 0.62, 0.26],
    coreB: [0.94, 0.44, 0.18],
    accent: [1, 0.65, 0.3],
    halo: [1, 0.65, 0.3],
    haloStrength: 0.5,
    rimStrength: 0.55,
    seed: 4.6,
  },
  teal_speaking: {
    coreA: [0.2, 0.85, 0.72],
    coreB: [0.28, 0.78, 0.4],
    accent: [0.3, 0.9, 0.6],
    halo: [0.3, 0.9, 0.6],
    haloStrength: 0.6,
    rimStrength: 0.6,
    seed: 5.8,
  },
  graphite: {
    coreA: [0.45, 0.45, 0.45],
    coreB: [0.28, 0.28, 0.28],
    accent: [0.52, 0.52, 0.52],
    halo: [0, 0, 0],
    haloStrength: 0,
    rimStrength: 0.25,
    seed: 0,
  },
} as const satisfies Record<string, AuroraPalette>;

export type AuroraPaletteKey = keyof typeof AURORA_PALETTES;

export function paletteFor(key: string | null | undefined): AuroraPalette {
  return AURORA_PALETTES[key as AuroraPaletteKey] ?? AURORA_PALETTES.graphite;
}
