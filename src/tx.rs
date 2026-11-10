//! Probe transactions: one per path per round, all signed by the same fee payer.

use anyhow::{Context, Result};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_message::{AccountMeta, Message};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;

/// What every copy in a round shares.
pub struct RoundTx<'a> {
    pub payer: &'a Keypair,
    pub blockhash: Hash,
    pub cu_limit: u32,
    pub cu_price_micro_lamports: u64,
    pub lock_account: Option<Pubkey>,
}

pub struct Probe {
    pub signature: Signature,
    pub wire: Vec<u8>,
}

/// One copy: compute budget, a self-transfer, and the path's tip if it has one.
/// The self-transfer moves `copy + 1` lamports, so copies with different `copy`
/// numbers have distinct signatures even with the same blockhash and payer. The fee payer is writable in every
/// copy, so copies of a round always contend for the same write lock; the optional
/// lock account is appended to the self-transfer as an unused writable account.
pub fn build(round: &RoundTx<'_>, copy: usize, tip: Option<(Pubkey, u64)>) -> Result<Probe> {
    let payer = round.payer.pubkey();
    let amount = u64::try_from(copy).context("copy index")? + 1;
    let mut transfer = solana_system_interface::instruction::transfer(&payer, &payer, amount);
    if let Some(lock) = round.lock_account {
        transfer.accounts.push(AccountMeta::new(lock, false));
    }
    let mut instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(round.cu_limit),
        ComputeBudgetInstruction::set_compute_unit_price(round.cu_price_micro_lamports),
        transfer,
    ];
    if let Some((account, lamports)) = tip {
        instructions.push(solana_system_interface::instruction::transfer(
            &payer, &account, lamports,
        ));
    }
    let message = Message::new(&instructions, Some(&payer));
    let tx = Transaction::new(&[round.payer], message, round.blockhash.clone());
    let wire = wincode::serialize(&tx).context("serializing probe transaction")?;
    Ok(Probe {
        signature: tx.signatures[0],
        wire,
    })
}

/// Parse a secret key given as base58 or as `solana-keygen`'s JSON byte array.
pub fn parse_keypair(secret: &str) -> Result<Keypair> {
    let secret = secret.trim();
    let bytes: Vec<u8> = if secret.starts_with('[') {
        serde_json::from_str(secret)
            .map_err(|_| anyhow::anyhow!("keypair JSON is not a byte array"))?
    } else {
        bs58::decode(secret)
            .into_vec()
            .map_err(|_| anyhow::anyhow!("keypair is neither base58 nor a JSON byte array"))?
    };
    Keypair::try_from(bytes.as_slice()).map_err(|_| anyhow::anyhow!("keypair must be 64 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn round<'a>(payer: &'a Keypair, lock: Option<Pubkey>) -> RoundTx<'a> {
        RoundTx {
            payer,
            blockhash: Hash::new_from_array([7; 32]),
            cu_limit: 1_000,
            cu_price_micro_lamports: 2_000_000,
            lock_account: lock,
        }
    }

    fn decode(wire: &[u8]) -> Transaction {
        wincode::deserialize(wire).unwrap()
    }

    /// Write locks as the message header requests them.
    fn writable(tx: &Transaction, i: usize) -> bool {
        let h = &tx.message.header;
        let signed = usize::from(h.num_required_signatures);
        if i < signed {
            i < signed - usize::from(h.num_readonly_signed_accounts)
        } else {
            i < tx.message.account_keys.len() - usize::from(h.num_readonly_unsigned_accounts)
        }
    }

    #[test]
    fn copies_have_distinct_signatures_and_share_the_payer_lock() {
        let payer = Keypair::new();
        let r = round(&payer, None);
        let a = build(&r, 0, None).unwrap();
        let b = build(&r, 1, None).unwrap();
        assert_ne!(a.signature, b.signature);
        for probe in [&a, &b] {
            let tx = decode(&probe.wire);
            assert_eq!(tx.message.account_keys[0], payer.pubkey());
            assert!(writable(&tx, 0));
            assert_eq!(tx.signatures.len(), 1);
            assert_eq!(tx.signatures[0], probe.signature);
        }
    }

    #[test]
    fn tip_and_lock_accounts_are_writable() {
        let payer = Keypair::new();
        let lock = Pubkey::new_unique();
        let tip = Pubkey::from_str("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5").unwrap();
        let probe = build(&round(&payer, Some(lock)), 2, Some((tip, 20_000))).unwrap();
        let tx = decode(&probe.wire);
        let keys = &tx.message.account_keys;
        for key in [lock, tip] {
            let i = keys.iter().position(|k| *k == key).unwrap();
            assert!(writable(&tx, i), "{key} must be writable");
        }
        assert_eq!(tx.message.instructions.len(), 4);
        assert!(probe.wire.len() < 1232);
    }

    #[test]
    fn parses_both_keypair_formats() {
        let k = Keypair::new();
        let json = serde_json::to_string(&k.to_bytes().to_vec()).unwrap();
        assert_eq!(parse_keypair(&json).unwrap().pubkey(), k.pubkey());
        let b58 = bs58::encode(k.to_bytes()).into_string();
        assert_eq!(parse_keypair(&b58).unwrap().pubkey(), k.pubkey());
        assert!(parse_keypair("not a key").is_err());
    }
}
