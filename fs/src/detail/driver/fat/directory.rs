//! FAT filesystem implementation of DirectoryOperations
use crate::LibUserResult;
use sunrise_libuser::error::{Error, FileSystemError};
use storage_device::StorageDevice;
use crate::interface::filesystem::*;

use libfat::directory::dir_entry::DirectoryEntry as FatDirectoryEntry;
use libfat::directory::dir_entry_iterator::DirectoryEntryIterator as FatDirectoryEntryIterator;
use super::error::from_driver;
use core::fmt;
use spin::Mutex;
use alloc::sync::Arc;
use alloc::boxed::Box;

use sunrise_libuser::fs::{DirectoryEntry, DirectoryEntryType};
use libfat::FileSystemIterator;

use arrayvec::ArrayString;

/// Predicate helper used to filter directory entries.
pub struct DirectoryFilterPredicate;

impl DirectoryFilterPredicate {
    /// Accept all entries except "." & "..".
    pub fn all(entry: &FatDirectoryEntry) -> bool {
        let name = entry.file_name.as_str();
        name != "." && name != ".."
    }

    /// Only accept directory entries.
    pub fn dirs(entry: &FatDirectoryEntry) -> bool {
        entry.attribute.is_directory() && Self::all(entry)
    }

    /// Only accept file entries.
    pub fn files(entry: &FatDirectoryEntry) -> bool {
        !entry.attribute.is_directory() && Self::all(entry)
    }
}

/// A libfat directory reader implementing ``DirectoryOperations``.
pub struct DirectoryInterface {
    /// The opened directory path. Used to get the complete path of every entries.
    base_path: ArrayString<[u8; PATH_LEN]>,

    /// libfat filesystem interface.
    inner_fs: Arc<Mutex<libfat::filesystem::FatFileSystem<Box<dyn StorageDevice<Error = Error> + Send>>>>,

    /// The iterator used to iter over libfat's directory entries.
    internal_iter: FatDirectoryEntryIterator,

    /// The filter required by the user.
    filter_fn: fn(&FatDirectoryEntry) -> bool,

    /// The number of entries in the directory after ``filter_fn``.
    entry_count: u64,
}

impl fmt::Debug for DirectoryInterface {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("DirectoryInterface")
           .field("base_path", &&self.base_path[..])
           .field("entry_count", &self.entry_count)
           .finish()
    }
}

impl<'a> DirectoryInterface {
    /// Create a new DirectoryInterface.
    pub fn new(base_path: ArrayString<[u8; PATH_LEN]>, inner_fs: Arc<Mutex<libfat::filesystem::FatFileSystem<Box<dyn StorageDevice<Error = Error> + Send>>>>, internal_iter: FatDirectoryEntryIterator, filter_fn: fn(&FatDirectoryEntry) -> bool, entry_count: u64) -> Self {
        DirectoryInterface { base_path, inner_fs, internal_iter, filter_fn, entry_count }
    }

    /// convert libfat's DirectoryEntry to libfs's DirectoryEntry.
    fn convert_entry(
        fat_dir_entry: FatDirectoryEntry,
        base_path: &ArrayString<[u8; PATH_LEN]>,
    ) -> LibUserResult<DirectoryEntry> {
        let mut path_str: ArrayString<[u8; PATH_LEN]> = ArrayString::new();

        let file_size = fat_dir_entry.file_size;

        let directory_entry_type = if fat_dir_entry.attribute.is_directory() {
            DirectoryEntryType::Directory
        } else {
            DirectoryEntryType::File
        };

        if path_str.try_push_str(base_path.as_str()).is_err() || path_str.try_push_str(fat_dir_entry.file_name.as_str()).is_err() {
            return Err(FileSystemError::InvalidInput.into())
        }

        let mut path = [0x0; PATH_LEN];

        let path_str_slice = path_str.as_bytes();
        path[..path_str_slice.len()].copy_from_slice(path_str_slice);

        Ok(DirectoryEntry {
            path,
            // We don't support the archive bit so we always return 0.
            attribute: 0,
            directory_entry_type,
            file_size: u64::from(file_size),
        })
    }
}

impl DirectoryOperations for DirectoryInterface {
    fn read(&mut self, buf: &mut [DirectoryEntry]) -> LibUserResult<u64> {
        for (index, entry) in buf.iter_mut().enumerate() {
            let mut raw_dir_entry;
            loop {
                let filesystem = self.inner_fs.lock();
                let entry_opt = self.internal_iter.next(&filesystem);

                // Prematury ending
                if entry_opt.is_none() {
                    return Ok(index as u64);
                }

                raw_dir_entry = entry_opt.unwrap().map_err(from_driver)?;
                let filter_fn = self.filter_fn;

                if filter_fn(&raw_dir_entry) {
                    break;
                }
            }

            *entry = Self::convert_entry(
                raw_dir_entry,
                &self.base_path,
            )?;
        }

        // everything was read correctly
        Ok(buf.len() as u64)
    }

    fn entry_count(&self) -> LibUserResult<u64> {
        Ok(self.entry_count)
    }
}