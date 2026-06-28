//! Bankineco vault integration for Titan's routing layer.
//!
//! Bankineco (Perena) is a multi-asset yield vault on Solana. Users deposit
//! whitelisted stablecoin base assets (USDC, USDT, PYUSD, …) in exchange for
//! share tokens (USD*), or burn shares to redeem base assets. The exchange rate
//! is set by the vault's on-chain NAV oracle, not by reserves — making this a
//! **linear fixed-price AMM** where the marginal price is constant regardless of
//! trade size.
//!
//! ## Integration points
//!
//! - [`parse_pool_creations`]: detects vault creation (`create_vault`) in
//!   confirmed transactions and returns the vault PDA + share/asset mints.
//! - [`YourVenue`]: implements [`TradingVenue`] for a single vault, supporting
//!   deposit (base asset → shares) and withdrawal (shares → base asset) for
//!   every whitelisted holding in the vault.
//!
//! ## Quote math
//!
//! ```text
//! out_gross = in * price_in * 10^dec_out / (price_out * 10^dec_in)
//! fee       = out_gross * fee_bps / 10_000
//! out_net   = out_gross - fee
//! price     = out_net / in  (constant — the curve is linear)
//! ```
//!
//! where `price_in` / `price_out` are the vault's 6-decimal fixed-point asset
//! prices in the accounting unit (1_000_000 = 1.00).

use ahash::HashSet;
use async_trait::async_trait;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use vault_sdk::Vault;

use crate::{
    account_caching::AccountsCache,
    trading_venue::{
        FromAccount, QuoteRequest, QuoteResult, TradingVenue,
        error::TradingVenueError,
        protocol::PoolProtocol,
        token_info::{TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, TokenInfo},
        venue_creation::{ParsedInstruction, PoolCreation},
    },
};

// Production program and vault — commented out until the prod program migration
// is complete. Switch YOUR_PROGRAM_ID and the active vault constant below once
// the on-chain program has been officially migrated.
//
// pub const YOUR_PROGRAM_ID: Pubkey =
//     Pubkey::from_str_const("save8RQVPMWNTzU18t3GBvBkN9hT7jsGjiCQ28FpD9H");
// pub const PROD_VAULT: Pubkey =
//     Pubkey::from_str_const("ECJGrTZ6QYMEwiEAnL4oReWF126uc22e9Lojy9qyCjHT"); // vault_id = 0

/// Test/staging Bankineco vault program ID (active).
pub const YOUR_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("6HyT8NQDpXY5wGkvX7haQVJ5nGUBVXQSkaT6Nf7fbsuJ");

/// Test/staging vault instance.
pub const TEST_VAULT: Pubkey =
    Pubkey::from_str_const("Bzj2KQqSaUB9QAWmdz1r4HttLjtGi5UQFTJrLx1B5hYK");

// Anchor instruction discriminators: sha256("global:<name>")[..8]
const EXECUTE_DEPOSIT_DISC: [u8; 8] = [247, 103, 46, 184, 88, 188, 56, 46];
// Always use execute_withdraw_from_external for withdrawals; it accepts optional
// InstructionRefs/slot-index args and supports Marginfi-backed liquidity.
// execute_withdraw is equivalent to calling this with (None, None) but we use
// execute_withdraw_from_external consistently to match the AMM interface.
const EXECUTE_WITHDRAW_FROM_EXTERNAL_DISC: [u8; 8] = [91, 38, 26, 250, 138, 227, 18, 88];
const CREATE_VAULT_DISC: [u8; 8] = [29, 237, 247, 208, 193, 82, 54, 135];

// PDA seeds from the vault program's common crate.
const VAULT_ORACLE_SEED: &[u8] = b"vault_oracle";
const FEE_VAULT_SEED: &[u8] = b"VFEEVAULT";
const VAULT_TRANCHE_SEED: &[u8] = b"vault_tranche";

const MARGINFI_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA");
const MAIN_MARGINFI_GROUP: Pubkey =
    Pubkey::from_str_const("4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8");

const SYSTEM_PROGRAM_ID: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJe1bJ8");

/// Per-mint Marginfi bank configuration (mainnet).
///
/// Seeds (derived from the Marginfi program):
///   bank_liquidity_vault_authority : ["liquidity_vault_auth", bank]
///   bank_liquidity_vault            : ["liquidity_vault",      bank]
struct MarginfiMintConfig {
    bank: Pubkey,
    liquidity_vault: Pubkey,
    liquidity_vault_auth: Pubkey,
}

const MARGINFI_USDC: MarginfiMintConfig = MarginfiMintConfig {
    bank: Pubkey::from_str_const("2s37akK2eyBbp8DZgCm7RtsaEz8eJP3Nxd4urLHQv7yB"),
    liquidity_vault: Pubkey::from_str_const("7jaiZR5Sk8hdYN9MxTpczTcwbWpb5WEoxSANuUwveuat"),
    liquidity_vault_auth: Pubkey::from_str_const("3uxNepDbmkDNq6JhRja5Z8QwbTrfmkKP8AKZV5chYDGG"),
};
const MARGINFI_USDT: MarginfiMintConfig = MarginfiMintConfig {
    bank: Pubkey::from_str_const("HmpMfL8942u22htC4EMiandCNCtkoFtyytu6aTFZMoiD"),
    liquidity_vault: Pubkey::from_str_const("4tFJXnPFMWnqFBYBhd3FnBMWMM4PJJmqcCH4ZYrCFvNe"),
    liquidity_vault_auth: Pubkey::from_str_const("7sXoVHHR7SLRB9Cz3EHjSM3M1JBoqB6fVLSmjVYTATxB"),
};
const MARGINFI_PYUSD: MarginfiMintConfig = MarginfiMintConfig {
    bank: Pubkey::from_str_const("8UEiPmgZHXXEDrqLS3oiTxQxTbeYTtPbeMBxAd2XGbpu"),
    liquidity_vault: Pubkey::from_str_const("ENnfVnYcbKZN57mUYCvsMiNUXZ8m2Dc1HETyfNDD66A8"),
    liquidity_vault_auth: Pubkey::from_str_const("582VxpQGLfUJRsdPYU2Q8dVLn1uxx9BuPMvtgwseB662"),
};
const MARGINFI_USDG: MarginfiMintConfig = MarginfiMintConfig {
    bank: Pubkey::from_str_const("Dj2CwMF3GM7mMT5hcyGXKuYSQ2kQ5zaVCkA1zX1qaTva"),
    liquidity_vault: Pubkey::from_str_const("5Euy1GJaWcF8BcZa2wbvKZq9ZU95anedL9TW416ZJNpK"),
    liquidity_vault_auth: Pubkey::from_str_const("J2RutaNtmw5Ri32iiZTexxNYHyDqJKbt6gVWCmv6hmnx"),
};
const MARGINFI_USDS: MarginfiMintConfig = MarginfiMintConfig {
    bank: Pubkey::from_str_const("FDsf8sj6SoV313qrA91yms3u5b3P4hBxEPvanVs8LtJV"),
    liquidity_vault: Pubkey::from_str_const("26uoGkHSxBSL2oMcpdMZT7pss6wsiVCgFw6US58YZggd"),
    liquidity_vault_auth: Pubkey::from_str_const("2bqe5Zdkw7zsyWZ2prmWgPbr3LfMCYEDNSqizTw2BqKL"),
};
const MARGINFI_CASH: MarginfiMintConfig = MarginfiMintConfig {
    bank: Pubkey::from_str_const("F4brCRJHx8epWah7p8Ace4ehutphxYZ1ctRq2LS3iiBh"),
    liquidity_vault: Pubkey::from_str_const("BogSuoRVycg5VSKSXi9YGjajhZ5uwCDA4HVPATEQXYVq"),
    liquidity_vault_auth: Pubkey::from_str_const("2nbp41Q7xN9wtomgoP3APtanSvqTg5PfYyNafPyABBp6"),
};

fn marginfi_config_for_mint(mint: &Pubkey) -> Option<&'static MarginfiMintConfig> {
    const USDC: Pubkey = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    const USDT: Pubkey = Pubkey::from_str_const("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB");
    const PYUSD: Pubkey = Pubkey::from_str_const("2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo");
    const USDG: Pubkey = Pubkey::from_str_const("GbMiMDYFX9sVMNQFmqmgKMhWfBvPNTJjxz4YubDKtDKE");
    const USDS: Pubkey = Pubkey::from_str_const("USDSwr9ApdHk5bvJKMjzff41FfuX8bSxdKcR81vTwcA");
    const CASH: Pubkey = Pubkey::from_str_const("CASHVDm2wsJXfhj6VWxb7GiMdoLc17Du7paH4bNr5woT");
    match *mint {
        USDC => Some(&MARGINFI_USDC),
        USDT => Some(&MARGINFI_USDT),
        PYUSD => Some(&MARGINFI_PYUSD),
        USDG => Some(&MARGINFI_USDG),
        USDS => Some(&MARGINFI_USDS),
        CASH => Some(&MARGINFI_CASH),
        _ => None,
    }
}

/// Serialize `InstructionRefs` for a single Marginfi withdraw CPI.
///
/// Wire format (all fields borsh `Vec<u8>`):
///   CpiMapping.indices  : [0..8] — 9 remaining accounts in order
///   CpiMapping.lengths  : [9]    — one CPI consuming all 9 accounts
///   CpiRefs.types       : [3]    — CpiType::MARGINFI_WITHDRAW
///   CpiRefs.args        : [0xFF, 0xFF] — Skip sentinel; amount resolved on-chain
///   InstructionRefs.tracked : []
fn build_marginfi_ix_refs() -> Vec<u8> {
    fn borsh_vec(v: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&(v.len() as u32).to_le_bytes());
        out.extend_from_slice(v);
    }
    let mut out = Vec::with_capacity(33);
    borsh_vec(&[0, 1, 2, 3, 4, 5, 6, 7, 8], &mut out);
    borsh_vec(&[9], &mut out);
    borsh_vec(&[3], &mut out);
    borsh_vec(&[0xFF, 0xFF], &mut out);
    borsh_vec(&[], &mut out);
    out
}

const BPS: u128 = 10_000;

/// Detect every Bankineco vault created in a confirmed transaction.
///
/// Titan tracks new vaults live by feeding decompiled transaction instructions
/// here. Each returned [`PoolCreation::pool`] is the vault PDA address, which
/// is then passed to [`YourVenue::from_account`] to build the quoting state.
///
/// `create_vault` account layout:
/// ```text
///   0: curator (signer / payer)
///   1: vault PDA       ← PoolCreation::pool
///   2: vault_oracle PDA
///   3: fee_vault PDA
///   4: share_mint
///   5: asset_mint      ← present in Strict mode; absent in Whitelist mode
///   …
/// ```
pub fn parse_pool_creations(instructions: &[ParsedInstruction]) -> Vec<PoolCreation> {
    const VAULT_IDX: usize = 1;
    const SHARE_MINT_IDX: usize = 4;
    const ASSET_MINT_IDX: usize = 5;

    instructions
        .iter()
        .filter(|ix| ix.program_id == YOUR_PROGRAM_ID)
        .filter(|ix| {
            ix.data
                .get(..8)
                .map(|d| d == CREATE_VAULT_DISC)
                .unwrap_or(false)
        })
        .filter_map(|ix| {
            let pool = *ix.accounts.get(VAULT_IDX)?;
            let share_mint = *ix.accounts.get(SHARE_MINT_IDX)?;
            let mints = match ix.accounts.get(ASSET_MINT_IDX) {
                Some(&asset_mint) => vec![share_mint, asset_mint],
                None => vec![share_mint],
            };
            Some(PoolCreation {
                protocol: PoolProtocol::PerenaVault,
                pool,
                mints,
            })
        })
        .collect()
}

/// Marginal price for a bankineco vault swap (output atoms per input atom).
///
/// The vault is a linear fixed-price AMM: the price is constant for any swap
/// size, satisfying Titan's monotonicity and mean-value-theorem requirements
/// trivially (a constant derivative on a linear output curve).
fn marginal_price(
    is_deposit: bool,
    share_price: u64,
    share_decimals: u8,
    asset_price: u64,
    asset_decimals: u8,
    fee_bps: u16,
) -> f64 {
    let (price_in, dec_in, price_out, dec_out) = if is_deposit {
        (
            asset_price as f64,
            asset_decimals as i32,
            share_price as f64,
            share_decimals as i32,
        )
    } else {
        (
            share_price as f64,
            share_decimals as i32,
            asset_price as f64,
            asset_decimals as i32,
        )
    };
    let rate = price_in * 10f64.powi(dec_out) / (price_out * 10f64.powi(dec_in));
    rate * (1.0 - fee_bps as f64 / BPS as f64)
}

/// Compute net output atoms for an exact-in bankineco vault swap.
fn calc_out(
    is_deposit: bool,
    in_amount: u64,
    share_price: u64,
    share_decimals: u8,
    asset_price: u64,
    asset_decimals: u8,
    fee_bps: u16,
) -> Option<u64> {
    if share_price == 0 {
        return None;
    }
    let (price_in, dec_in, price_out, dec_out) = if is_deposit {
        (
            asset_price as u128,
            asset_decimals,
            share_price as u128,
            share_decimals,
        )
    } else {
        (
            share_price as u128,
            share_decimals,
            asset_price as u128,
            asset_decimals,
        )
    };
    let numerator = (in_amount as u128)
        .checked_mul(price_in)?
        .checked_mul(10u128.pow(dec_out as u32))?;
    let denominator = price_out.checked_mul(10u128.pow(dec_in as u32))?;
    let out_gross = numerator.checked_div(denominator)?;
    let fee = out_gross * fee_bps as u128 / BPS;
    out_gross.checked_sub(fee)?.try_into().ok()
}

/// Bankineco vault quoting venue.
///
/// Wraps a single vault instance. Supports deposit (base asset → USD* shares)
/// and withdrawal (USD* shares → base asset) for every whitelisted holding in
/// the vault. The share mint occupies `token_info[0]`; base asset mints follow.
#[derive(Clone)]
pub struct YourVenue {
    /// Address of the vault account (the "pool" in Titan terminology).
    pub pool_id: Pubkey,
    token_info: Vec<TokenInfo>,
    required_state_pubkeys: HashSet<Pubkey>,
    initialized: bool,

    // Snapshot of vault state — refreshed in update_state.
    share_mint: Pubkey,
    share_decimals: u8,
    /// vault.accounting.mint_share_price — 6-decimal fixed-point.
    share_price: u64,
    mint_fee_bps: u16,
    burn_fee_bps: u16,
    tranching_enabled: bool,
    /// Active Marginfi external-liquidity position: (user_account, slot_index).
    /// `None` when the vault has no Marginfi liquidity deployed.
    marginfi_position: Option<(Pubkey, u8)>,

    /// Whitelisted base holdings: (mint, price, decimals, is_token_2022).
    base_holdings: Vec<(Pubkey, u64, u8, bool)>,
}

impl YourVenue {
    fn build_from_vault(pool_id: Pubkey, vault: &Vault) -> Self {
        let share_mint = Pubkey::from(vault.mint);

        let base_holdings: Vec<(Pubkey, u64, u8, bool)> = vault
            .holdings
            .iter()
            .filter(|h| h.is_base == 1 && h.mint != [0u8; 32])
            .map(|h| {
                // TokenProgram: Spl = 0, Token2022 = 1
                (Pubkey::from(h.mint), h.price, h.decimals, h.token_program == 1)
            })
            .collect();

        // Share mint (USD*) first at index 0, base holdings follow.
        let mut token_info = vec![TokenInfo {
            pubkey: share_mint,
            decimals: vault.mint_decimals as i32,
            is_token_2022: false, // USD* is a standard SPL Token
            transfer_fee: None,
            maximum_fee: None,
        }];
        for &(mint, _, decimals, is_token_2022) in &base_holdings {
            token_info.push(TokenInfo {
                pubkey: mint,
                decimals: decimals as i32,
                is_token_2022,
                transfer_fee: None,
                maximum_fee: None,
            });
        }

        // Detect an active Marginfi external-liquidity position.
        // Layout of ExternalLiquiditySlot (common::state::external_liquidity):
        //   [0]     source discriminant (1 = Marginfi)
        //   [1..8]  _padding
        //   [8..40] user_account (Pubkey)
        let marginfi_position = vault.external_liquidity
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.data[0] == 1)
            .and_then(|(i, slot)| {
                let bytes: [u8; 32] = slot.data[8..40].try_into().ok()?;
                let pk = Pubkey::from(bytes);
                if pk == Pubkey::default() { None } else { Some((pk, i as u8)) }
            });

        let mut required_state_pubkeys = HashSet::default();
        required_state_pubkeys.insert(pool_id);

        YourVenue {
            pool_id,
            token_info,
            required_state_pubkeys,
            initialized: false,
            share_mint,
            share_decimals: vault.mint_decimals,
            share_price: vault.accounting.mint_share_price,
            mint_fee_bps: vault.config.fees.mint_fee_bps,
            burn_fee_bps: vault.config.fees.burn_fee_bps,
            tranching_enabled: vault.tranching_enabled == 1,
            marginfi_position,
            base_holdings,
        }
    }

    fn base_holding_for(&self, mint: &Pubkey) -> Option<(u64, u8, bool)> {
        self.base_holdings
            .iter()
            .find(|(m, ..)| m == mint)
            .map(|&(_, price, decimals, is_tok22)| (price, decimals, is_tok22))
    }

    fn find_pda(&self, seeds: &[&[u8]]) -> Pubkey {
        Pubkey::find_program_address(seeds, &YOUR_PROGRAM_ID).0
    }

    fn token_program_for(&self, mint: &Pubkey) -> Pubkey {
        self.token_info
            .iter()
            .find(|ti| &ti.pubkey == mint)
            .map(|ti| {
                if ti.is_token_2022 {
                    TOKEN_2022_PROGRAM_ID
                } else {
                    TOKEN_PROGRAM_ID
                }
            })
            .unwrap_or(TOKEN_PROGRAM_ID)
    }
}

impl FromAccount for YourVenue {
    fn from_account(pubkey: &Pubkey, account: &Account) -> Result<Self, TradingVenueError> {
        let vault = Vault::from_account_data(&account.data).map_err(|_| {
            TradingVenueError::FromAccountError(
                "invalid Vault discriminator or layout".into(),
            )
        })?;
        Ok(YourVenue::build_from_vault(*pubkey, &vault))
    }
}

#[async_trait]
impl TradingVenue for YourVenue {
    fn initialized(&self) -> bool {
        self.initialized
    }

    fn program_id(&self) -> Pubkey {
        YOUR_PROGRAM_ID
    }

    fn program_dependencies(&self) -> Vec<Pubkey> {
        let mut deps = vec![YOUR_PROGRAM_ID];
        if self.marginfi_position.is_some() {
            deps.push(MARGINFI_PROGRAM_ID);
        }
        deps
    }

    fn market_id(&self) -> Pubkey {
        self.pool_id
    }

    fn get_token_info(&self) -> &[TokenInfo] {
        &self.token_info
    }

    fn protocol(&self) -> PoolProtocol {
        PoolProtocol::PerenaVault
    }

    fn get_required_pubkeys_for_update(&self) -> Result<Vec<Pubkey>, TradingVenueError> {
        Ok(self.required_state_pubkeys.iter().cloned().collect())
    }

    async fn update_state(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        let accounts = cache.get_accounts(&[self.pool_id]).await?;
        let vault_account = accounts
            .into_iter()
            .next()
            .flatten()
            .ok_or_else(|| TradingVenueError::MissingState(self.pool_id.into()))?;

        let vault = Vault::from_account_data(&vault_account.data)
            .map_err(|_| TradingVenueError::DeserializationFailed(self.pool_id.into()))?;

        // Rebuild all derived state from the refreshed vault account.
        let updated = YourVenue::build_from_vault(self.pool_id, &vault);
        self.share_mint = updated.share_mint;
        self.share_decimals = updated.share_decimals;
        self.share_price = updated.share_price;
        self.mint_fee_bps = updated.mint_fee_bps;
        self.burn_fee_bps = updated.burn_fee_bps;
        self.tranching_enabled = updated.tranching_enabled;
        self.marginfi_position = updated.marginfi_position;
        self.base_holdings = updated.base_holdings;
        self.token_info = updated.token_info;
        self.initialized = true;
        Ok(())
    }

    fn quote(&self, request: QuoteRequest) -> Result<QuoteResult, TradingVenueError> {
        let is_deposit = request.input_mint != self.share_mint;
        let asset_mint = if is_deposit {
            request.input_mint
        } else {
            request.output_mint
        };

        let (asset_price, asset_decimals, _) = self
            .base_holding_for(&asset_mint)
            .ok_or_else(|| TradingVenueError::InvalidMint(asset_mint.into()))?;

        let fee_bps = if is_deposit {
            self.mint_fee_bps
        } else {
            self.burn_fee_bps
        };

        let price = marginal_price(
            is_deposit,
            self.share_price,
            self.share_decimals,
            asset_price,
            asset_decimals,
            fee_bps,
        );

        // Zero-input: return spot price with zero output.
        if request.amount == 0 {
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: false,
                price,
            });
        }

        let expected_output = calc_out(
            is_deposit,
            request.amount,
            self.share_price,
            self.share_decimals,
            asset_price,
            asset_decimals,
            fee_bps,
        )
        .ok_or_else(|| TradingVenueError::MissingState("quote math overflow".into()))?;

        Ok(QuoteResult {
            input_mint: request.input_mint,
            output_mint: request.output_mint,
            amount: request.amount,
            expected_output,
            not_enough_liquidity: false,
            price,
        })
    }

    fn generate_swap_instruction(
        &self,
        request: QuoteRequest,
        user: Pubkey,
    ) -> Result<Instruction, TradingVenueError> {
        let is_deposit = request.input_mint != self.share_mint;
        let asset_mint = if is_deposit {
            request.input_mint
        } else {
            request.output_mint
        };

        let asset_token_program = self.token_program_for(&asset_mint);
        // USD* (share mint) is always a standard SPL Token.
        let share_token_program = TOKEN_PROGRAM_ID;

        let vault_oracle =
            self.find_pda(&[VAULT_ORACLE_SEED, self.pool_id.as_ref()]);
        let fee_vault = self.find_pda(&[FEE_VAULT_SEED, self.pool_id.as_ref()]);

        let user_asset_ata =
            get_associated_token_address_with_program_id(&user, &asset_mint, &asset_token_program);
        let vault_asset_ata = get_associated_token_address_with_program_id(
            &self.pool_id,
            &asset_mint,
            &asset_token_program,
        );
        let fee_vault_ata = get_associated_token_address_with_program_id(
            &fee_vault,
            &asset_mint,
            &asset_token_program,
        );
        let user_share_ata = get_associated_token_address_with_program_id(
            &user,
            &self.share_mint,
            &share_token_program,
        );

        let mut accounts = vec![
            AccountMeta::new(user, true), // signer flag cleared by route builder for PDA
            AccountMeta::new(self.pool_id, false),
            AccountMeta::new_readonly(vault_oracle, false),
        ];

        // vault_tranche_state is only required when tranching is enabled.
        if self.tranching_enabled {
            let tranche =
                self.find_pda(&[VAULT_TRANCHE_SEED, self.pool_id.as_ref()]);
            accounts.push(AccountMeta::new_readonly(tranche, false));
        }

        accounts.extend_from_slice(&[
            AccountMeta::new_readonly(asset_mint, false),
            AccountMeta::new(self.share_mint, false),
            AccountMeta::new(user_asset_ata, false),
            AccountMeta::new(vault_asset_ata, false),
            AccountMeta::new(fee_vault, false),
            AccountMeta::new(fee_vault_ata, false),
            AccountMeta::new(user_share_ata, false),
            AccountMeta::new_readonly(asset_token_program, false),
            AccountMeta::new_readonly(share_token_program, false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ]);

        // Instruction data:
        //   execute_deposit(amount: u64):
        //     disc(8) + amount(8)
        //   execute_withdraw_from_external(share_amount, ix_refs: Option<_>, source: Option<u8>):
        //     disc(8) + amount(8) + borsh(Option<InstructionRefs>) + borsh(Option<u8>)
        let mut data = if is_deposit {
            let mut d = EXECUTE_DEPOSIT_DISC.to_vec();
            d.extend_from_slice(&request.amount.to_le_bytes());
            d
        } else {
            let mut d = EXECUTE_WITHDRAW_FROM_EXTERNAL_DISC.to_vec();
            d.extend_from_slice(&request.amount.to_le_bytes());

            // Append Marginfi accounts and encode (Some(InstructionRefs), Some(slot))
            // if the vault has an active Marginfi position for this asset mint.
            let marginfi_args = self.marginfi_position
                .and_then(|(marginfi_account, slot_index)| {
                    let config = marginfi_config_for_mint(&asset_mint)?;
                    Some((marginfi_account, slot_index, config))
                });

            if let Some((marginfi_account, slot_index, config)) = marginfi_args {
                // Option::Some(InstructionRefs) — borsh discriminant 1 + payload
                d.push(1);
                d.extend_from_slice(&build_marginfi_ix_refs());
                // Option::Some(slot_index)
                d.push(1);
                d.push(slot_index);

                accounts.extend_from_slice(&[
                    AccountMeta::new_readonly(MARGINFI_PROGRAM_ID, false),
                    AccountMeta::new(MAIN_MARGINFI_GROUP, false),
                    AccountMeta::new(marginfi_account, false),
                    AccountMeta::new_readonly(self.pool_id, false),
                    AccountMeta::new(config.bank, false),
                    AccountMeta::new(vault_asset_ata, false),
                    AccountMeta::new_readonly(config.liquidity_vault_auth, false),
                    AccountMeta::new(config.liquidity_vault, false),
                    AccountMeta::new_readonly(asset_token_program, false),
                ]);
            } else {
                // No Marginfi: Option::None for both args
                d.push(0); // ix_refs = None
                d.push(0); // source = None
            }
            d
        };
        Ok(Instruction {
            program_id: YOUR_PROGRAM_ID,
            accounts,
            data,
        })
    }
}
