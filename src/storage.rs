//! Provide storage options based on files in a data directory or google sheets with periodic
//! polling.

use std::path::PathBuf;

use eyre::Result;
use secrecy::{SecretBox, zeroize::Zeroize};
use serde::{Serialize, de::DeserializeOwned};

pub struct FileStorage<R>
where
    R: Serialize + DeserializeOwned + Zeroize,
{
    path: PathBuf,
    /// initial value when file does not exist
    initial: SecretBox<R>,
}

impl<R> FileStorage<R>
where
    R: Serialize + DeserializeOwned + Zeroize,
{
    pub fn new(name: String, initial: R) -> Self {
        todo!()
    }

    pub async fn load(&self) -> Result<R> {
        todo!()
    }

    pub async fn write(&self, record: R) -> Result<R> {
        todo!()
    }
}
