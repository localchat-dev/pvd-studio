use crate::Result;
use std::{
    fs::{File, OpenOptions},
    io::{self, Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

pub struct Input {
    data: Data,
    length: u64,
}

enum Data {
    File {
        file: File,
        path: PathBuf,
    },
    Archive {
        reader: Cursor<Vec<u8>>,
        member: String,
        archive_bytes: u64,
    },
}

impl Input {
    pub fn open(path: impl AsRef<Path>, progress: impl FnMut(u64, u64) -> bool) -> Result<Self> {
        let path = path.as_ref();
        Self::from_file(path, File::open(path)?, progress)
    }

    pub fn open_locked(
        path: impl AsRef<Path>,
        progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<Self> {
        let path = path.as_ref();
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        Self::from_file(path, options.open(path)?, progress)
    }

    fn from_file(
        path: &Path,
        mut file: File,
        progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<Self> {
        let length = file.metadata()?.len();
        let mut prefix = [0; 4];
        if length >= 4 {
            file.read_exact(&mut prefix)?;
            file.rewind()?;
        }
        if pvd_core::pvd_archive::has_zip_signature(&prefix) {
            let archive = pvd_core::pvd_archive::read_with_progress(
                &mut file,
                &Default::default(),
                progress,
            )?;
            Ok(Self {
                length: archive.bytes.len() as u64,
                data: Data::Archive {
                    reader: Cursor::new(archive.bytes),
                    member: archive.member_name,
                    archive_bytes: length,
                },
            })
        } else {
            Ok(Self {
                data: Data::File {
                    file,
                    path: path.to_owned(),
                },
                length,
            })
        }
    }

    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn archive(&self) -> Option<(&str, u64)> {
        match &self.data {
            Data::Archive {
                member,
                archive_bytes,
                ..
            } => Some((member, *archive_bytes)),
            Data::File { .. } => None,
        }
    }

    pub fn is_native(&mut self) -> Result<bool> {
        self.rewind()?;
        let mut prefix = [0; 4];
        let result = self.read_exact(&mut prefix);
        self.rewind()?;
        result?;
        Ok(prefix == pvd_core::native::MAGIC.to_le_bytes())
    }

    pub fn native_recording(
        &mut self,
        progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<pvd_core::Recording> {
        let result = match &mut self.data {
            Data::File { path, .. } => {
                pvd_core::native::read_path_with_progress(path, Default::default(), progress)
            }
            Data::Archive { reader, .. } => pvd_core::native::read_bytes_with_history(
                reader.get_ref(),
                Default::default(),
                progress,
            ),
        };
        result.map(|(recording, _)| recording)
    }
}

impl Read for Input {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match &mut self.data {
            Data::File { file, .. } => file.read(bytes),
            Data::Archive { reader, .. } => reader.read(bytes),
        }
    }
}

impl Seek for Input {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match &mut self.data {
            Data::File { file, .. } => file.seek(position),
            Data::Archive { reader, .. } => reader.seek(position),
        }
    }
}

#[cfg(test)]
mod tests;
