export interface Point {
  x: number;
  y: number;
}

/**
 * Format chart values for constrained axis and bar labels.
 *
 * Full absolute values are still available through native title/tooltips in
 * chart components; visible labels prefer compact notation so they do not clip
 * the plot area.
 */
export function formatCompactChartNumber(value: number, maximumFractionDigits: number = 1): string {
  if (!Number.isFinite(value)) return '-';
  const absolute = Math.abs(value);
  if (absolute >= 1000) {
    return new Intl.NumberFormat('en-US', {
      notation: 'compact',
      maximumFractionDigits
    }).format(value);
  }
  if (absolute === 0) return '0';
  if (absolute < 1) {
    return value.toFixed(2).replace(/\.?0+$/, '');
  }
  if (absolute < 10) {
    return value.toFixed(1).replace(/\.0$/, '');
  }
  return Math.round(value).toLocaleString('en-US');
}

export function formatSignedCompactChartDelta(value: number): string {
  const normalized = Math.abs(value) < 1e-9 ? 0 : value;
  if (normalized === 0) return '0';
  const prefix = normalized > 0 ? '+' : '-';
  return `${prefix}${formatCompactChartNumber(Math.abs(normalized))}`;
}

function niceAxisStep(value: number): number {
  const absolute = Math.abs(value);
  if (!Number.isFinite(absolute) || absolute <= 0) return 1;
  const magnitude = Math.pow(10, Math.floor(Math.log10(absolute)));
  const normalized = absolute / magnitude;
  const nice = normalized >= 5 ? 5 : normalized >= 2 ? 2 : 1;
  return nice * magnitude;
}

export function referenceBaseForChartAxis(minValue: number, maxValue: number): number {
  if (!Number.isFinite(minValue) || !Number.isFinite(maxValue)) return 0;
  if (minValue <= 0 && maxValue >= 0) return 0;

  const span = Math.abs(maxValue - minValue);
  const maxAbsolute = Math.max(Math.abs(minValue), Math.abs(maxValue));
  if (maxAbsolute < 1000 || span <= 0 || span / maxAbsolute > 0.5) return 0;

  const step = niceAxisStep(Math.max(maxAbsolute / 10, span / 4));
  const base = minValue > 0
    ? Math.floor(minValue / step) * step
    : Math.ceil(maxValue / step) * step;

  return Math.abs(base) >= 1000 ? base : 0;
}

/**
 * Generates an ASCII representation of a line chart.
 */
export function generateAsciiLineChart(
  points: Point[],
  width: number = 60,
  height: number = 15
): string {
  if (points.length === 0) return 'No data';

  // Find boundaries
  const minX = Math.min(...points.map(p => p.x));
  const maxX = Math.max(...points.map(p => p.x));
  const minY = Math.min(...points.map(p => p.y));
  const maxY = Math.max(...points.map(p => p.y));

  const rangeX = maxX - minX || 1;
  const rangeY = maxY - minY || 1;

  // Initialize grid
  const grid: string[][] = Array.from({ length: height }, () =>
    Array.from({ length: width }, () => ' ')
  );

  // Map points to grid
  const mappedPoints = points.map(p => ({
    x: Math.round(((p.x - minX) / rangeX) * (width - 1)),
    y: Math.round(((p.y - minY) / rangeY) * (height - 1))
  }));

  // Sort by x
  mappedPoints.sort((a, b) => a.x - b.x);

  // Draw points and simple lines (interpolated)
  for (let i = 0; i < mappedPoints.length; i++) {
    const p1 = mappedPoints[i];
    grid[height - 1 - p1.y][p1.x] = '*';

    if (i < mappedPoints.length - 1) {
      const p2 = mappedPoints[i + 1];
      // Simple linear interpolation for connecting lines
      const steps = Math.max(Math.abs(p2.x - p1.x), Math.abs(p2.y - p1.y));
      for (let s = 1; s < steps; s++) {
        const ix = Math.round(p1.x + (p2.x - p1.x) * (s / steps));
        const iy = Math.round(p1.y + (p2.y - p1.y) * (s / steps));
        if (grid[height - 1 - iy][ix] === ' ') {
          grid[height - 1 - iy][ix] = '.';
        }
      }
    }
  }

  // Build the result string with axes
  let result = '';
  for (let y = 0; y < height; y++) {
    // Y-axis value
    const val = maxY - (y / (height - 1)) * rangeY;
    result += val.toFixed(1).padStart(6) + ' | ' + grid[y].join('') + '\n';
  }

  // X-axis
  result += ' '.repeat(7) + '+' + '-'.repeat(width) + '\n';
  
  // X-axis labels
  const labelStart = minX.toString();
  const labelEnd = maxX.toString();
  result += ' '.repeat(7) + labelStart + ' '.repeat(width - labelStart.length - labelEnd.length + 1) + labelEnd;

  return result;
}

/**
 * Generates an ASCII bar chart.
 */
export function generateAsciiBarChart(
  data: { label: string; value: number }[],
  width: number = 40
): string {
  if (data.length === 0) return 'No data';

  const maxValue = Math.max(...data.map(d => d.value));
  const maxLabelLength = Math.max(...data.map(d => d.label.length));

  return data.map(d => {
    const barLength = Math.round((d.value / maxValue) * width);
    const bar = '#'.repeat(barLength);
    return `${d.label.padEnd(maxLabelLength)} | ${bar} (${d.value})`;
  }).join('\n');
}

/**
 * Extracts a numeric value from DuckDB-Wasm types.
 * DuckDB-Wasm can serialize large numbers into objects or strings like "HugeInt(xxx)".
 */
export function parseNumericDuckDbValue(value: unknown): number | undefined {
  if (value == null) return undefined;
  if (typeof value === 'number') return Number.isFinite(value) ? value : undefined;
  if (typeof value === 'bigint') return Number(value);

  let str = typeof value === 'string' ? value : String(value);
  const match = str.match(/^(?:HugeInt|Decimal|BigInt)\((.*)\)$/);
  if (match) str = match[1];

  const num = Number(str);
  return Number.isFinite(num) ? num : undefined;
}

/**
 * Cleans string output for DuckDB-Wasm types to avoid displaying "HugeInt(xxx)" to users.
 */
export function cleanDuckDbValue(value: unknown): unknown {
  if (value == null) return value;
  if (typeof value !== 'string' && typeof value !== 'object') return value;
  
  const str = String(value);
  const match = str.match(/^(?:HugeInt|Decimal|BigInt)\((.*)\)$/);
  if (match) {
      // Return a string or a Number depending on context, string is safest for Table rendering
      return match[1];
  }
  return value;
}
