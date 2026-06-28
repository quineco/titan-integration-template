//! Bankineco vault test suite — the same shared assertions the example passes,
//! run against `YourVenue` (the Bankineco integration). Tests SKIP when
//! `SOLANA_RPC_URL` is unset or when program binaries are missing from
//! `programs/` (see `make dump-programs`).

mod common;

use common::SuiteConfig;
use solana_pubkey::{Pubkey, pubkey};
use titan_integration_template::your_venue::{TEST_VAULT, YOUR_PROGRAM_ID, YourVenue};

// Installs the allocation guard that powers the construction test's
// `assert_no_alloc` checks. The Makefile runs that test under `release-debug`
// so the guard is active; speed tests run under true `--release`.
#[cfg(debug_assertions)]
#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn pool() -> Pubkey {
    TEST_VAULT
}

fn programs() -> Vec<Pubkey> {
    // The vault program binary must be dumped to programs/<id>.so.
    // Run `make dump-programs` (or `solana program dump <id> programs/<id>.so`)
    // before executing the simulation-backed tests.
    vec![YOUR_PROGRAM_ID]
}

fn config() -> SuiteConfig {
    SuiteConfig {
        pool: pool(),
        programs: programs(),
    }
}

#[tokio::test]
async fn construction() {
    common::construction::<YourVenue>(&config()).await;
}

#[tokio::test]
async fn zero_input_spot_price() {
    common::zero_input_spot_price::<YourVenue>(&config()).await;
}

#[tokio::test]
async fn bound_simulation() {
    common::bound_simulation::<YourVenue>(&config()).await;
}

#[tokio::test]
async fn random_samples() {
    common::random_samples::<YourVenue>(&config()).await;
}

#[tokio::test]
async fn monotone() {
    common::monotone::<YourVenue>(&config()).await;
}

#[tokio::test]
async fn quoting_speed() {
    common::quoting_speed::<YourVenue>(&config()).await;
}

#[tokio::test]
async fn price_monotone() {
    common::price_monotone::<YourVenue>(&config()).await;
}

#[tokio::test]
async fn mean_value_theorem() {
    common::mean_value_theorem::<YourVenue>(&config()).await;
}
