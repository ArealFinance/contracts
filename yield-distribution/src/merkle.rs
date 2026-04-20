//! Canonical (lower-hash-first) SHA256 merkle proof verification + leaf hashing.
//!
//! Both the verifier (on-chain) and the tree-builder (off-chain) MUST use byte-level
//! `<=` comparison for canonical ordering. Any off-chain implementation must mirror
//! this exact logic.

use sha2::{Digest, Sha256};

/// Verify a merkle proof.
/// Each step hashes the lower of (current, sibling) first to produce a canonical tree.
pub fn verify_merkle_proof(proof: &[[u8; 32]], root: &[u8; 32], leaf: &[u8; 32]) -> bool {
    let mut current = *leaf;
    for sibling in proof {
        let mut hasher = Sha256::new();
        if current <= *sibling {
            hasher.update(&current);
            hasher.update(sibling);
        } else {
            hasher.update(sibling);
            hasher.update(&current);
        }
        let out = hasher.finalize();
        current.copy_from_slice(&out[..32]);
    }
    current == *root
}

/// Leaf = sha256(claimant || cumulative_amount_le)
pub fn compute_leaf(claimant: &[u8; 32], cumulative_amount: u64) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(claimant);
    hasher.update(&cumulative_amount.to_le_bytes());
    let out = hasher.finalize();
    let mut leaf = [0u8; 32];
    leaf.copy_from_slice(&out[..32]);
    leaf
}
