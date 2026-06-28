//! Enumeration of supported pool/AMM protocol types.
//!
//! Each `TradingVenue` declares which protocol it implements (e.g. a specific
//! AMM, orderbook, or proprietary liquidity engine). Titan uses this enum to
//! label venues, group similar pools, and provide protocol-specific routing or
//! heuristics where applicable.

use std::fmt::Display;

/// Identifies the protocol family or implementation style of a trading venue.
///
/// Every AMM or custom pool that integrates with Titan must choose one of these
/// variants (or add their own) so the router and UI can correctly identify and
/// categorize the venue.
///
/// Protocols included here:
/// - `PerenaBankineco`: Perena's multi-asset yield vault (linear fixed-price AMM).
/// - `RaydiumAMM`: Raydium's constant-product AMM on Solana.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PoolProtocol {
    /// Perena vault — a multi-asset yield vault that issues USD* shares against
    /// whitelisted stablecoin deposits at a fixed oracle-priced exchange rate.
    PerenaVault,

    /// Raydium's AMM (x*y=k) pools on Solana.
    RaydiumAMM,
}

impl Display for PoolProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from(*self))
    }
}

impl From<PoolProtocol> for String {
    fn from(protocol: PoolProtocol) -> Self {
        match protocol {
            PoolProtocol::PerenaVault => "PerenaVault".to_string(),
            PoolProtocol::RaydiumAMM => "RaydiumAMM".to_string(),
        }
    }
}
