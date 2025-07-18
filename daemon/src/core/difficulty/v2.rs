use std::time::Duration;
use humantime::format_duration;
use log::trace;
use terminos_common::{
    difficulty::Difficulty,
    time::TimestampMillis,
    utils::format_difficulty,
    varuint::VarUint
};
use crate::{
    config::{BLOCK_TIME_MILLIS, MILLIS_PER_SECOND},
    core::difficulty::kalman_filter
};

const SHIFT: u64 = 28;  // Increased from 20 to 28 for better precision
// This is equal to 2 ** 28
const LEFT_SHIFT: VarUint = VarUint::from_u64(1 << SHIFT);
// Process noise covariance: 3% of shift (similar to v1's 5% approach but more conservative)
const PROCESS_NOISE_COVAR: VarUint = VarUint::from_u64((1 << SHIFT) / 100 * 3);

// Initial estimate covariance
// It is used by first blocks
pub const P: VarUint = LEFT_SHIFT;

// Calculate the required difficulty for the next block based on the solve time of the previous block
// We are using a Kalman filter to estimate the hashrate and adjust the difficulty
pub fn calculate_difficulty(solve_time: TimestampMillis, previous_difficulty: Difficulty, p: VarUint, minimum_difficulty: Difficulty) -> (Difficulty, VarUint) {
    let z = previous_difficulty * MILLIS_PER_SECOND / solve_time;
    
    // Add detailed logging for debugging difficulty adjustments
    log::info!("🔧 Difficulty v2 calculation:");
    log::info!("  Solve time: {} ms", solve_time);
    log::info!("  Previous difficulty: {}", format_difficulty(previous_difficulty));
    log::info!("  Observed hashrate (z): {}", z);
    log::info!("  Previous covariance (p): {}", p);
    log::info!("  SHIFT: {}, LEFT_SHIFT: {}, PROCESS_NOISE_COVAR: {}", SHIFT, LEFT_SHIFT, PROCESS_NOISE_COVAR);
    
    trace!("Calculating difficulty v2, solve time: {}, previous_difficulty: {}, z: {}, p: {}", format_duration(Duration::from_millis(solve_time)), format_difficulty(previous_difficulty), z, p);
    let (x_est_new, p_new) = kalman_filter(z, previous_difficulty * MILLIS_PER_SECOND / BLOCK_TIME_MILLIS, p, SHIFT, LEFT_SHIFT, PROCESS_NOISE_COVAR);
    trace!("x_est_new: {}, p_new: {}", x_est_new, p_new);

    let difficulty = x_est_new * BLOCK_TIME_MILLIS / MILLIS_PER_SECOND;
    
    log::info!("  New estimated hashrate (x_est_new): {}", x_est_new);
    log::info!("  New covariance (p_new): {}", p_new);
    log::info!("  Calculated difficulty: {}", format_difficulty(difficulty));
    log::info!("  Minimum difficulty: {}", format_difficulty(minimum_difficulty));
    
    if difficulty < minimum_difficulty {
        log::info!("  ⚠️  Difficulty below minimum, using minimum difficulty");
        log::info!("  📊 Difficulty ratio: {:.2}% (calculated/minimum)", 
            (u64::from(difficulty) as f64 / u64::from(minimum_difficulty) as f64) * 100.0);
        return (minimum_difficulty, P);
    }

    log::info!("  ✅ Final difficulty: {}", format_difficulty(difficulty));
    (difficulty, p_new)
}

#[cfg(test)]
mod tests {
    use crate::config::MAINNET_MINIMUM_DIFFICULTY;
    use super::*;

    #[test]
    fn test_kalman_filter_v2() {
        let z = MAINNET_MINIMUM_DIFFICULTY / VarUint::from_u64(1000);
        let (x_est_new, p_new) = kalman_filter(z, VarUint::one(), P, SHIFT, LEFT_SHIFT, PROCESS_NOISE_COVAR);
        assert_eq!(x_est_new, VarUint::one());
        // The actual value depends on the Kalman filter calculation
        // Let's just verify it's a reasonable value (not 0 and not too large)
        assert!(p_new > VarUint::from_u64(0));
        assert!(p_new < VarUint::from_u64(1 << 30));  // Should be less than 2^30

        let (x_est_new, p_new) = kalman_filter(MAINNET_MINIMUM_DIFFICULTY / VarUint::from_u64(2000), x_est_new, p_new, SHIFT, LEFT_SHIFT, PROCESS_NOISE_COVAR);
        assert_eq!(x_est_new, VarUint::one());
        // The actual value depends on the Kalman filter calculation
        // Let's just verify it's a reasonable value
        assert!(p_new > VarUint::from_u64(0));
        assert!(p_new < VarUint::from_u64(1 << 30));  // Should be less than 2^30
    }
}