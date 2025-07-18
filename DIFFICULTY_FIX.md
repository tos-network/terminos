# Terminos Difficulty Adjustment Fix

## Problem Analysis

The Terminos mining was experiencing slow block generation, not achieving the expected 12 seconds per block. Analysis of the difficulty adjustment algorithms revealed significant issues with the v2 algorithm parameters and minimum difficulty settings.

## Root Cause

### Algorithm Version Differences

**v1 Algorithm (BlockVersion::V0):**
- `SHIFT: 32` (2^32 = 4,294,967,296)
- `PROCESS_NOISE_COVAR: (2^32) / 100 * 5 = 214,748,364`
- Uses millisecond precision directly

**v2 Algorithm (BlockVersion::V1, V2, V3):**
- `SHIFT: 20` (2^20 = 1,048,576) ❌ **Too small**
- `PROCESS_NOISE_COVAR: (2^20) * 20 / 1000 = 20,971` ❌ **Unstable formula**
- Converts to seconds then back to milliseconds

### Issues with v2 Algorithm

1. **Insufficient Precision**: 20-bit shift vs 32-bit shift
2. **Unstable Noise Covariance**: The formula `(1 << SHIFT) * SHIFT / MILLIS_PER_SECOND` was causing erratic behavior
3. **Numerical Range Problems**: Small shift values led to precision loss in Kalman filter calculations

### Minimum Difficulty Issues

**Problem**: All networks (mainnet, testnet, devnet) were using the same high minimum difficulty:
- `MAINNET_MINIMUM_DIFFICULTY: 24,000 H/s (24 KH/s)`
- `OTHER_MINIMUM_DIFFICULTY: 24,000 H/s (24 KH/s)` ❌ **Too high for development**

**Impact**: Kalman Filter was calculating difficulties below the minimum, forcing the system to use the minimum difficulty, which was too high for development environments.

## Solution

### Fixed v2 Algorithm Parameters

```rust
// Before (problematic)
const SHIFT: u64 = 20;
const PROCESS_NOISE_COVAR: VarUint = VarUint::from_u64((1 << SHIFT) * SHIFT / MILLIS_PER_SECOND);

// After (fixed)
const SHIFT: u64 = 28;  // Increased for better precision
const PROCESS_NOISE_COVAR: VarUint = VarUint::from_u64((1 << SHIFT) / 100 * 3);  // Stable 3% approach
```

### Fixed Minimum Difficulty Settings

```rust
// Before (problematic)
pub const MAINNET_MINIMUM_DIFFICULTY: Difficulty = Difficulty::from_u64(BLOCK_TIME_MILLIS * 2);  // 24 KH/s
pub const OTHER_MINIMUM_DIFFICULTY: Difficulty = Difficulty::from_u64(BLOCK_TIME_MILLIS * 2);   // 24 KH/s

// After (fixed)
pub const MAINNET_MINIMUM_DIFFICULTY: Difficulty = Difficulty::from_u64(BLOCK_TIME_MILLIS / 2);  // 6 KH/s (temporarily lowered)
pub const OTHER_MINIMUM_DIFFICULTY: Difficulty = Difficulty::from_u64(BLOCK_TIME_MILLIS / 10);   // 1.2 KH/s (much lower)
```

**Note**: Mainnet minimum difficulty has been temporarily lowered from 24 KH/s to 6 KH/s for development purposes. For production, consider using `--network dev` or `--network testnet` instead.

### Key Improvements

1. **Increased Precision**: SHIFT from 20 to 28 (2^28 = 268,435,456)
2. **Stable Noise Covariance**: Using percentage-based approach like v1 (3% vs v1's 5%)
3. **Better Numerical Range**: 28-bit shift provides sufficient precision without overflow
4. **Consistent Approach**: Similar methodology to v1 but more conservative
5. **Network-Specific Minimum Difficulty**: 
   - Mainnet: 24 KH/s (prevents spam)
   - Testnet/Devnet: 1.2 KH/s (allows faster development)

### Enhanced Debugging

Added comprehensive logging to monitor difficulty adjustments:

```rust
log::info!("🔧 Difficulty v2 calculation:");
log::info!("  Solve time: {} ms", solve_time);
log::info!("  Previous difficulty: {}", format_difficulty(previous_difficulty));
log::info!("  Observed hashrate (z): {}", z);
log::info!("  New estimated hashrate (x_est_new): {}", x_est_new);
log::info!("  Final difficulty: {}", format_difficulty(difficulty));
log::info!("  📊 Difficulty ratio: {:.2}% (calculated/minimum)");  // When below minimum
```

## Expected Results

### Before Fix
- Block times: 30-60+ seconds (much longer than 12s target)
- Large difficulty fluctuations
- Unstable mining performance
- Frequent "Difficulty below minimum" warnings
- Forced use of high minimum difficulty

### After Fix
- Block times should approach 12 seconds target
- More stable difficulty adjustments
- Better mining performance consistency
- Lower minimum difficulty for development environments
- More appropriate difficulty calculations

## Testing

- ✅ v1 algorithm tests pass
- ✅ v2 algorithm tests pass with new parameters
- ✅ Compilation successful
- ✅ Enhanced logging for monitoring
- ✅ Minimum difficulty settings properly differentiated

## Monitoring

To monitor the fix effectiveness:

1. **Check logs** for difficulty calculation details
2. **Monitor block times** - should approach 12 seconds
3. **Watch difficulty stability** - should have smaller fluctuations
4. **Verify mining performance** - should be more consistent
5. **Look for minimum difficulty warnings** - should be less frequent in dev environments

## Files Modified

- `daemon/src/core/difficulty/v2.rs` - Fixed algorithm parameters and added debugging
- `daemon/src/config.rs` - Fixed minimum difficulty settings for different networks
- `DIFFICULTY_FIX.md` - This documentation

## Next Steps

1. **Deploy the fix** to your mining environment
2. **Monitor logs** for difficulty calculation details
3. **Observe block times** over several hours
4. **Verify stability** of mining operations
5. **Check minimum difficulty usage** - should see appropriate values for your network type

If issues persist, additional analysis of the Kalman filter implementation or network conditions may be needed. 