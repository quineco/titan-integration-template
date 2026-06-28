//! Bankineco vault-creation parsing test.
//!
//! Builds a self-contained fixture for the vault program's `create_vault`
//! instruction and confirms that `parse_pool_creations` correctly detects it.
//! No RPC or network access — mirrors the shape of `tests/venue_creation.rs`.

use solana_pubkey::{Pubkey, pubkey};

use titan_integration_template::trading_venue::protocol::PoolProtocol;
use titan_integration_template::trading_venue::venue_creation::{ParsedInstruction, PoolCreation};
use titan_integration_template::your_venue::{YOUR_PROGRAM_ID, parse_pool_creations};

// Test/staging vault instance (vault_id = 0 on the staging program).
const VAULT: Pubkey = pubkey!("Bzj2KQqSaUB9QAWmdz1r4HttLjtGi5UQFTJrLx1B5hYK");

// Share token minted by the vault (USD*).
const SHARE_MINT: Pubkey = pubkey!("star9agSpjiFe3M49B3RniVU4CMBBEK3Qnaqn3RGiFM");

// Base asset mint used in the fixture (USDC — the vault's primary holding).
const ASSET_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

// Anchor discriminator: sha256("global:create_vault")[..8]
const CREATE_VAULT_DISC: [u8; 8] = [29, 237, 247, 208, 193, 82, 54, 135];

/// Build a `create_vault` instruction in its on-chain account layout.
///
/// Account ordering from `VaultCreateVault` in the vault program:
/// ```text
///   0: curator (signer / payer)
///   1: vault PDA
///   2: vault_oracle PDA
///   3: fee_vault PDA
///   4: share_mint
///   5: asset_mint (Strict mode — present here)
///   6: asset_token_program
///   7: token_program
///   8: system_program
/// ```
fn bankineco_create_vault(vault: Pubkey, share_mint: Pubkey, asset_mint: Pubkey) -> ParsedInstruction {
    // Instruction data: discriminator + placeholder VaultCreateVaultArgs bytes.
    // The parser only inspects the 8-byte discriminator prefix.
    let mut data = CREATE_VAULT_DISC.to_vec();
    data.extend_from_slice(&[0u8; 16]); // minimal placeholder args

    let mut accounts = vec![Pubkey::new_unique(); 9];
    accounts[1] = vault;
    accounts[4] = share_mint;
    accounts[5] = asset_mint;

    ParsedInstruction {
        program_id: YOUR_PROGRAM_ID,
        accounts,
        data,
    }
}

/// An unrelated instruction to the vault program (no pool-creation discriminator).
fn unrelated_instruction() -> ParsedInstruction {
    let mut data = vec![0u8; 8]; // wrong discriminator
    data.extend_from_slice(&[0u8; 8]);
    ParsedInstruction {
        program_id: YOUR_PROGRAM_ID,
        accounts: vec![],
        data,
    }
}

#[test]
fn parses_bankineco_vault_creation() {
    let instructions = vec![
        unrelated_instruction(),
        bankineco_create_vault(VAULT, SHARE_MINT, ASSET_MINT),
    ];

    let creations = parse_pool_creations(&instructions);

    assert_eq!(
        creations,
        vec![PoolCreation {
            protocol: PoolProtocol::PerenaVault,
            pool: VAULT,
            mints: vec![SHARE_MINT, ASSET_MINT],
        }],
    );
}

#[test]
fn ignores_transactions_without_a_creation() {
    let creations = parse_pool_creations(&[unrelated_instruction()]);
    assert!(
        creations.is_empty(),
        "a transaction without a vault creation creates no pools, got {creations:?}"
    );
}

#[test]
fn whitelist_mode_vault_creation_omits_asset_mint() {
    // In Whitelist mode, asset_mint is absent (index 5 not present).
    // parse_pool_creations should still return a PoolCreation with only the share_mint.
    let mut data = CREATE_VAULT_DISC.to_vec();
    data.extend_from_slice(&[0u8; 16]);

    let accounts = vec![
        Pubkey::new_unique(), // 0: curator
        VAULT,                // 1: vault
        Pubkey::new_unique(), // 2: vault_oracle
        Pubkey::new_unique(), // 3: fee_vault
        SHARE_MINT,           // 4: share_mint
        // no asset_mint at index 5
    ];

    let ix = ParsedInstruction {
        program_id: YOUR_PROGRAM_ID,
        accounts,
        data,
    };

    let creations = parse_pool_creations(&[ix]);
    assert_eq!(creations.len(), 1);
    assert_eq!(creations[0].pool, VAULT);
    assert_eq!(creations[0].mints, vec![SHARE_MINT]);
}
