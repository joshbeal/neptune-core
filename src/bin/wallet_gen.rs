use anyhow::Result;
use neptune_core::config_models::{data_directory::get_data_directory, network::Network};
use neptune_core::models::blockchain::wallet::{self, Wallet};

pub const WALLET_DIR: &str = "wallet.dat";

#[tokio::main]
async fn main() -> Result<()> {
    // The root path is where both the wallet and all databases are stored
    let root_data_dir_path = get_data_directory(Network::Main)?;

    // Create root directory for databases and wallet if it does not already exist
    std::fs::create_dir_all(&root_data_dir_path).unwrap_or_else(|err| {
        panic!(
            "Failed to create data directory in {}: {}",
            root_data_dir_path.to_string_lossy(),
            err
        )
    });
    let wallet_file = Wallet::wallet_path(&root_data_dir_path);
    let wallet = Wallet::initialize_wallet(
        &wallet_file,
        wallet::STANDARD_WALLET_NAME,
        wallet::STANDARD_WALLET_VERSION,
    );

    println!("Wallet stored in: {}", wallet_file.display());
    println!("Wallet public key: {}", wallet.get_public_key());

    Ok(())
}
