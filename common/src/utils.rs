use std::net::SocketAddr;
use log::warn;

use crate::{
    config::{
        COIN_DECIMALS,
        FEE_PER_ACCOUNT_CREATION,
        FEE_PER_KB,
        FEE_PER_TRANSFER,
        BYTES_PER_KB
    },
    difficulty::Difficulty,
    varuint::VarUint
};

pub mod energy_fee;

/// Static assert macro to check conditions at compile time
/// Usage: `static_assert!(condition);` or `static_assert!(condition, "Error message");`
#[macro_export]
macro_rules! static_assert {
    ($cond:expr $(,)?) => {
        const _: () = {
            assert!($cond);
        };
    };
    ($cond:expr, $($arg:tt)+) => {
        const _: () = {
            assert!($cond, $($arg)+);
        };
    };
}

#[macro_export]
macro_rules! async_handler {
    ($func: expr) => {
        move |a, b| {
          Box::pin($func(a, b))
        }
    };
}

// Format any coin value using the requested decimals count
pub fn format_coin(value: u64, decimals: u8) -> String {
    format!("{:.1$}", value as f64 / 10usize.pow(decimals as u32) as f64, decimals as usize)
}

// Format value using terminos decimals
pub fn format_terminos(value: u64) -> String {
    format_coin(value, COIN_DECIMALS)
}

// Convert a terminos amount from string to a u64
pub fn from_terminos(value: impl Into<String>) -> Option<u64> {
    from_coin(value, COIN_DECIMALS)
}

// Convert a coin amount from string to a u64 based on the provided decimals
pub fn from_coin(value: impl Into<String>, coin_decimals: u8) -> Option<u64> {
    let value = value.into();
    if value.is_empty() {
        return None;
    }

    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() > 2 {
        return None;
    }

    let integer_part = parts[0];
    let decimal_part = if parts.len() == 2 { parts[1] } else { "" };

    // Parse integer part
    let integer: u64 = integer_part.parse().ok()?;

    // Parse decimal part
    let mut decimal: u64 = 0;
    if !decimal_part.is_empty() {
        if decimal_part.len() > coin_decimals as usize {
            return None;
        }
        let mut padded_decimal = decimal_part.to_string();
        while padded_decimal.len() < coin_decimals as usize {
            padded_decimal.push('0');
        }
        decimal = padded_decimal.parse().ok()?;
    }

    Some(integer * 10u64.pow(coin_decimals as u32) + decimal)
}

// Format a TOS amount for display (8 decimals)
pub fn format_tos(value: u64) -> String {
    format_coin(value, COIN_DECIMALS)
}

// Convert a TOS amount from string to a u64
pub fn from_tos(value: impl Into<String>) -> Option<u64> {
    from_coin(value, COIN_DECIMALS)
}

// Detect the available parallelism
// Default to 1 on error
pub fn detect_available_parallelism() -> usize {
    match std::thread::available_parallelism() {
        Ok(n) => n.get(),
        Err(e) => {
            warn!("Error while detecting parallelism, default to 1: {}", e);
            1
        }
    }
}


// return the fee for a transaction based on its size in bytes
// the fee is calculated in atomic units for TOS
// Sending to a newly created address will increase the fee
// Each transfers output will also increase the fee
// Each signature of a multisig add a small overhead due to the verfications
pub fn calculate_tx_fee(tx_size: usize, output_count: usize, new_addresses: usize, multisig: usize) -> u64 {
    let mut size_in_kb = tx_size as u64 / BYTES_PER_KB as u64;

    // we consume a full kb for fee
    if tx_size % BYTES_PER_KB != 0 {
        size_in_kb += 1;
    }

    size_in_kb * FEE_PER_KB
    + output_count as u64 * FEE_PER_TRANSFER
    + new_addresses as u64 * FEE_PER_ACCOUNT_CREATION
    + multisig as u64 * FEE_PER_TRANSFER
}

// Calculate energy fee for a transaction (only transfer supported)
pub fn calculate_energy_fee(tx_size: usize, output_count: usize, new_addresses: usize) -> u64 {
    use crate::utils::energy_fee::EnergyFeeCalculator;
    
    // Only transfer operations consume energy, so we only need these 3 parameters
    EnergyFeeCalculator::calculate_energy_cost(
        tx_size,
        output_count,
        new_addresses
    )
}

const HASHRATE_FORMATS: [&str; 7] = ["H/s", "KH/s", "MH/s", "GH/s", "TH/s", "PH/s", "EH/s"];

// Format a hashrate in human-readable format
pub fn format_hashrate(mut hashrate: f64) -> String {
    let max = HASHRATE_FORMATS.len() - 1;
    let mut count = 0;
    while hashrate >= 1000f64 && count < max {
        count += 1;
        hashrate = hashrate / 1000f64;
    }

    return format!("{:.2} {}", hashrate, HASHRATE_FORMATS[count]);
}

const DIFFICULTY_FORMATS: [&str; 7] = ["", "K", "M", "G", "T", "P", "E"];

// Format a difficulty in a human-readable format
pub fn format_difficulty(mut difficulty: Difficulty) -> String {
    let max = HASHRATE_FORMATS.len() - 1;
    let mut count = 0;
    let thousand = VarUint::from_u64(1000);
    let mut left = VarUint::zero();
    while difficulty >= thousand && count < max {
        count += 1;
        left = difficulty % thousand;
        difficulty = difficulty / thousand;
    }

    let left_str = if left == VarUint::zero() {
        "".to_string()
    } else {
        format!(".{}", left / 10)
    };

    return format!("{}{}{}", difficulty, left_str, DIFFICULTY_FORMATS[count]);
}

// Sanitize a ws address to make sure it's a valid websocket address
// By default, will use ws:// if no protocol is specified
pub fn sanitize_ws_address(target: &str) -> String {
    let mut target = target.to_lowercase();
    if target.starts_with("https://") {
        target.replace_range(..8, "wss://");
    }
    else if target.starts_with("http://") {
        target.replace_range(..7, "ws://");
    }
    else if !target.starts_with("ws://") && !target.starts_with("wss://") {
        // use ws:// if it's a IP address, otherwise it may be a domain, use wss://
        let prefix = if target.parse::<SocketAddr>().is_ok() {
            "ws://"
        } else {
            "wss://"
        };

        target.insert_str(0, prefix);
    }

    if target.ends_with("/") {
        target.pop();
    }

    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::COIN_VALUE;

    #[test]
    fn test_format_coin() {
        assert_eq!(
            from_coin("10", 8),
            Some(10 * COIN_VALUE)
        );
        assert_eq!(
            from_coin("1", 8),
            Some(COIN_VALUE)
        );
        assert_eq!(
            from_coin("0.1", 8),
            Some(COIN_VALUE / 10)
        );
        assert_eq!(
            from_coin("0.01", 8),
            Some(COIN_VALUE / 100)
        );

        assert_eq!(
            from_coin("0.1", 1),
            Some(1)
        );
        assert_eq!(
            from_coin("1", 0),
            Some(1)
        );
    }

    #[test]
    fn test_terminos_format() {
        assert_eq!(format_terminos(FEE_PER_ACCOUNT_CREATION), "0.00100000");
        assert_eq!(format_terminos(FEE_PER_KB), "0.00010000");
        assert_eq!(format_terminos(FEE_PER_TRANSFER), "0.00005000");
        assert_eq!(format_terminos(COIN_VALUE), "1.00000000");
        assert_eq!(format_terminos(1), "0.00000001");
    }

    #[test]
    fn test_difficulty_format_zero() {
        let value = Difficulty::zero();
        assert_eq!(format_difficulty(value), "0");
    }

    #[test]
    fn test_difficulty_format_thousand_k() {
        let value: Difficulty = 1000u64.into();
        assert_eq!(format_difficulty(value), "1K");
    }

    #[test]
    fn test_difficulty_format_thousand_k_left() {
        let value: Difficulty = 1150u64.into();
        assert_eq!(format_difficulty(value), "1.15K");
    }

    #[test]
    fn test_high_difficulty() {
        let value: Difficulty = 1150_000_000u64.into();
        assert_eq!(format_difficulty(value), "1.15G");

        let max: Difficulty = u64::MAX.into();
        assert_eq!(format_difficulty(max), "18.44E");
    }

    #[test]
    fn test_from_terminos() {
        let value = from_terminos("100.123");
        assert_eq!(value, Some(100_123_00000));
    }
}