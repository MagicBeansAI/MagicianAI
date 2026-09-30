# Frontend Engineering Agent Memory

## Design System & Aesthetics
- **Framework & UI Library**: Use Next.js with Tailwind CSS and shadcn/ui components. (Or SvelteKit with DaisyUI, etc.)
- **Typography**: Prioritize maximum legibility, clear visual hierarchy, and precise typographic details (line-height, letter-spacing, and kerning). Use a modern sans-serif font like Inter or Roboto.
- **Colors**: Use a harmonious color palette (e.g., HSL tailored colors). Avoid harsh pure black or pure white backgrounds.
- **Layout**: Keep interfaces clean and uncluttered. Use generous whitespace. Avoid over-nesting cards.
- **Micro-interactions**: Incorporate subtle hover effects, active states, and transitions to make the interface feel responsive and alive.
- **Forbidden Cliché Design Tropes**: 
  - No dashboard overuse when not necessary.
  - No purple fonts or violet accents on dark theme backgrounds.
  - No colored border accents or glowing colored outlines.
  - No huge untracked typefaces without proper letter-spacing.
  - No textureless surfaces (lack of depth).
  - No icon-stuffed bento boxes.

## Visual Self-Correction
- Always verify your visual changes against the design specifications.
- Ensure the page is responsive and components adapt correctly to different screen sizes.

## Project Structure
- Place UI components in `components/ui`.
- Define common styles in `styles/globals.css`.
- Keep business logic separate from UI presentation.
