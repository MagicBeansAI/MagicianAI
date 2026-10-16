/**
 * Minimal typings for the yuka steering library (v0.7.x ships no types) —
 * declared for exactly the surface the Fleet Civilization engine uses.
 * Extend here as more behaviours (wander, cohesion, FSM…) come into play.
 */
declare module 'yuka' {
	export class Vector3 {
		x: number;
		y: number;
		z: number;
		constructor(x?: number, y?: number, z?: number);
		set(x: number, y: number, z: number): this;
		copy(v: Vector3): this;
		distanceTo(v: Vector3): number;
		squaredLength(): number;
	}

	export class Path {
		loop: boolean;
		add(waypoint: Vector3): this;
		clear(): this;
		current(): Vector3;
		finished(): boolean;
		advance(): this;
	}

	export class SteeringBehavior {
		active: boolean;
		weight: number;
	}

	export class FollowPathBehavior extends SteeringBehavior {
		path: Path;
		nextWaypointDistance: number;
		constructor(path?: Path, nextWaypointDistance?: number);
	}

	export class SeparationBehavior extends SteeringBehavior {
		constructor();
	}

	export class SteeringManager {
		add(behavior: SteeringBehavior): this;
	}

	export class GameEntity {
		position: Vector3;
		updateNeighborhood: boolean;
		neighborhoodRadius: number;
	}

	export class MovingEntity extends GameEntity {
		velocity: Vector3;
		maxSpeed: number;
	}

	export class Vehicle extends MovingEntity {
		maxForce: number;
		steering: SteeringManager;
	}

	export class EntityManager {
		add(entity: GameEntity): this;
		remove(entity: GameEntity): this;
		update(delta: number): this;
	}
}
