use crate::error::VcsError;
use camino::Utf8PathBuf;
use std::borrow::Cow;
use std::fs;
use std::io::{self, BufWriter, Read, Write};

mod private { pub trait Sealed {} }

pub trait ObjectStore: private::Sealed + Send + Sync {
    fn read(&self, hash_hex: &str) -> Result<Cow<'_, [u8]>, VcsError>;
    fn write(&self, hash_hex: &str, reader: &mut dyn Read) -> Result<(), VcsError>;
    fn exists(&self, hash_hex: &str) -> bool;
}

pub struct FsObjectStore {
    root: Utf8PathBuf,
}

impl FsObjectStore {
    pub fn new(root: Utf8PathBuf) -> Self { Self { root } }
}

impl private::Sealed for FsObjectStore {}

impl ObjectStore for FsObjectStore {
    fn read(&self, hash_hex: &str) -> Result<Cow<'_, [u8]>, VcsError> {
        let path = self.root.join(&hash_hex[..2]).join(&hash_hex[2..]);
        let compressed = fs::read(&path)?;
        Ok(Cow::Owned(zstd::decode_all(&compressed[..])?))
    }

    fn write(&self, hash_hex: &str, reader: &mut dyn Read) -> Result<(), VcsError> {
        let dir = self.root.join(&hash_hex[..2]);
        fs::create_dir_all(&dir)?;
        let path = dir.join(&hash_hex[2..]);
        if path.exists() { return Ok(()); }

        let tmp_path = path.with_extension("tmp");
        let file = fs::File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        let mut encoder = zstd::Encoder::new(writer, 3)?;
        io::copy(reader, &mut encoder)?;
        encoder.finish()?;
        fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    fn exists(&self, hash_hex: &str) -> bool {
        self.root.join(&hash_hex[..2]).join(&hash_hex[2..]).exists()
    }
}
