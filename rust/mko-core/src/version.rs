pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const KNOWLEDGE_CONTRACT_VERSION: &str = "0.1.0";

pub fn supports_contract(version: &str) -> bool {
    version == KNOWLEDGE_CONTRACT_VERSION
}
