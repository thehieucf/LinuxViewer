//! Partner passwords in the desktop keyring (Secret Service: GNOME Keyring,
//! KWallet, KeePassXC…). Only stored when the user chooses to remember them.

use std::collections::HashMap;

use secret_service::{EncryptionType, SecretService};

const APPLICATION: &str = "linuxviewer";

#[derive(Debug, Clone)]
pub struct CredentialError(pub String);

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn error(e: secret_service::Error) -> CredentialError {
    CredentialError(format!("The keyring is unavailable ({e})."))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_digit())
}

/// Runs on the tokio runtime (the D-Bus connection needs it).
pub async fn password(id: String) -> Result<Option<String>, CredentialError> {
    if !valid_id(&id) {
        return Ok(None);
    }
    let service = SecretService::connect(EncryptionType::Dh).await.map_err(error)?;
    let attributes = HashMap::from([("application", APPLICATION), ("partner", id.as_str())]);
    let found = service.search_items(attributes).await.map_err(error)?;
    let item = match (found.unlocked.into_iter().next(), found.locked.into_iter().next()) {
        (Some(item), _) => item,
        (None, Some(item)) => {
            item.unlock().await.map_err(error)?;
            item
        }
        (None, None) => return Ok(None),
    };
    let secret = item.get_secret().await.map_err(error)?;
    Ok(String::from_utf8(secret).ok())
}

pub async fn store(id: String, password: String) -> Result<(), CredentialError> {
    if !valid_id(&id) {
        return Ok(());
    }
    let service = SecretService::connect(EncryptionType::Dh).await.map_err(error)?;
    let collection = service.get_default_collection().await.map_err(error)?;
    collection.ensure_unlocked().await.map_err(error)?;
    let attributes = HashMap::from([("application", APPLICATION), ("partner", id.as_str())]);
    collection
        .create_item(&format!("LinuxViewer partner {id}"), attributes, password.as_bytes(), true, "text/plain")
        .await
        .map_err(error)?;
    Ok(())
}

pub async fn remove_all() -> Result<(), CredentialError> {
    let service = SecretService::connect(EncryptionType::Dh).await.map_err(error)?;
    let found = service.search_items(HashMap::from([("application", APPLICATION)])).await.map_err(error)?;
    for item in found.unlocked.iter().chain(found.locked.iter()) {
        item.delete().await.map_err(error)?;
    }
    Ok(())
}
