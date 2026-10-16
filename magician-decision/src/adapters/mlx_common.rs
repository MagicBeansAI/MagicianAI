//! What every MLX model in the engine shares.

/// MLX keeps freed buffers for reuse, by default up to ~75% of RAM. Every
/// request here has its own shapes, so little is reused and the cache only
/// grows: a Kev-0.8B engine reached a 37 GB footprint over 40 varied steps
/// with 1.4 GB resident. The limit is process-wide; the engine is the
/// process's owner, so it is set here. `DECISION_MLX_CACHE_LIMIT_MB`
/// overrides it.
const MLX_CACHE_LIMIT: usize = 512 << 20;

fn mlx_cache_limit() -> usize {
    std::env::var("DECISION_MLX_CACHE_LIMIT_MB")
        .ok()
        .and_then(|mb| mb.trim().parse::<usize>().ok())
        .map_or(MLX_CACHE_LIMIT, |mb| mb << 20)
}

/// Apply the process-wide MLX allocator cache limit (idempotent; every MLX
/// model's worker calls it before loading).
pub(crate) fn limit_mlx_cache() -> Result<(), mlx_rs::error::Exception> {
    mlx_rs::memory::set_cache_limit(mlx_cache_limit()).map(|_| ())
}
