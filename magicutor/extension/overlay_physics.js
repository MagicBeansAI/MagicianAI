/**
 * Physics engine for visual overlay.
 * Handles spring simulation and path generation for natural cursor movement.
 */

// Spring presets
export const SPRING_PRESETS = {
    snappy: { mass: 1.0, stiffness: 170, damping: 26 },  // Default - quick, minimal bounce
    smooth: { mass: 1.0, stiffness: 120, damping: 20 },  // Slower, slightly bouncy
    bouncy: { mass: 1.0, stiffness: 200, damping: 10 },  // Playful, visible overshoot
    gentle: { mass: 1.5, stiffness: 100, damping: 30 },  // Slow, no bounce
};

export class Spring {
    constructor(config = SPRING_PRESETS.snappy) {
        this.mass = config.mass;
        this.stiffness = config.stiffness;
        this.damping = config.damping;
        
        // State
        this.x = 0;
        this.y = 0;
        this.vx = 0; // Velocity X
        this.vy = 0; // Velocity Y
        this.targetX = 0;
        this.targetY = 0;
    }

    setTarget(x, y) {
        this.targetX = x;
        this.targetY = y;
    }

    // Teleport without physics
    snapTo(x, y) {
        this.x = x;
        this.y = y;
        this.targetX = x;
        this.targetY = y;
        this.vx = 0;
        this.vy = 0;
    }

    // Step simulation
    // dt: delta time in seconds
    update(dt) {
        // Force max dt to prevent explosion on lag spikes
        const timeStep = Math.min(dt, 0.05);

        // Spring force: F = -kx
        const fx = -this.stiffness * (this.x - this.targetX);
        const fy = -this.stiffness * (this.y - this.targetY);

        // Damping force: F = -cv
        const dx = -this.damping * this.vx;
        const dy = -this.damping * this.vy;

        // Acceleration: a = F/m
        const ax = (fx + dx) / this.mass;
        const ay = (fy + dy) / this.mass;

        // Update velocity
        this.vx += ax * timeStep;
        this.vy += ay * timeStep;

        // Update position
        this.x += this.vx * timeStep;
        this.y += this.vy * timeStep;

        // Check if settled
        const isResting = 
            Math.abs(this.vx) < 0.1 && 
            Math.abs(this.vy) < 0.1 && 
            Math.abs(this.x - this.targetX) < 0.1 && 
            Math.abs(this.y - this.targetY) < 0.1;

        return isResting;
    }
}

/**
 * Generate a Bezier curve path for natural movement
 * Adds randomization and perpendicular offset
 */
export class BezierPath {
    constructor(startX, startY, endX, endY) {
        this.start = { x: startX, y: startY };
        this.end = { x: endX, y: endY };
        
        const dist = Math.hypot(endX - startX, endY - startY);
        
        // Don't curve short paths (<100px)
        if (dist < 100) {
            this.control1 = this.start;
            this.control2 = this.end;
            return;
        }

        // Add variance
        const offset = Math.min(30, dist * 0.2); // Cap offset
        const variance = (Math.random() - 0.5) * offset * 2;
        
        // Calculate perpendicular vector
        const dx = endX - startX;
        const dy = endY - startY;
        const perpX = -dy / dist;
        const perpY = dx / dist;

        // Control point 1: 25% of way + offset
        this.control1 = {
            x: startX + dx * 0.25 + perpX * variance,
            y: startY + dy * 0.25 + perpY * variance
        };

        // Control point 2: 75% of way - offset (S-curve) or same offset (C-curve)
        // Let's do C-curve mostly
        this.control2 = {
            x: startX + dx * 0.75 + perpX * variance,
            y: startY + dy * 0.75 + perpY * variance
        };
    }

    // Get point at t (0..1)
    getPoint(t) {
        const invT = 1 - t;
        const invT2 = invT * invT;
        const invT3 = invT2 * invT;
        const t2 = t * t;
        const t3 = t2 * t;

        // Cubic Bezier formula
        const x = invT3 * this.start.x +
                  3 * invT2 * t * this.control1.x +
                  3 * invT * t2 * this.control2.x +
                  t3 * this.end.x;

        const y = invT3 * this.start.y +
                  3 * invT2 * t * this.control1.y +
                  3 * invT * t2 * this.control2.y +
                  t3 * this.end.y;

        return { x, y };
    }
}
