use ethers_core::utils::hex;
use sha3::{Digest, Keccak256};

/// A normalized address in checksum format.
pub type Address = String;

/// Converts an address to checksum format and returns a typed instance.
pub fn to_address(s: impl AsRef<str>) -> Address {
    let mut address = s.as_ref().to_ascii_lowercase();
    let hash = &Keccak256::digest(&address);
    hex::encode(hash)
}
