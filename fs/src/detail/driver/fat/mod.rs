//! FAT driver implementation layer

use alloc::boxed::Box;
use crate::LibUserResult;
use crate::interface::driver::FileSystemDriver;
use crate::interface::filesystem::FileSystemOperations;

use libfat;
use libfat::FatFsType;

mod directory;
mod file;
mod filesystem;
mod error;

use storage_device::StorageDevice;

use sunrise_libuser::fs::FileSystemType;
use sunrise_libuser::error::{Error, FileSystemError};
use filesystem::FatFileSystem;

use error::from_driver;

/// A FAT driver.
pub struct FATDriver;

impl FileSystemDriver for FATDriver {
    fn construct(&self, storage: Box<dyn StorageDevice<Error = Error> + Send>) -> LibUserResult<Box<dyn FileSystemOperations>> {
        let filesystem_instance = FatFileSystem::from_storage(storage)?;
        Ok(Box::new(filesystem_instance) as Box<dyn FileSystemOperations>)
    }

    fn probe(&self, storage: &mut (dyn StorageDevice<Error = Error> + Send)) -> Option<FileSystemType> {
        libfat::get_fat_type(storage, 0).ok().and_then(|filesytem_type| {
            match filesytem_type {
                FatFsType::Fat12 => Some(FileSystemType::FAT12),
                FatFsType::Fat16 => Some(FileSystemType::FAT16),
                FatFsType::Fat32 => Some(FileSystemType::FAT32)
            }
        })
    }

    fn is_supported(&self, filesytem_type: FileSystemType) -> bool {
        match filesytem_type {
            FileSystemType::FAT12 | FileSystemType::FAT16 | FileSystemType::FAT32 => true,
            _ => false
        }
    }

    fn format(&self, storage: Box<dyn StorageDevice<Error = Error> + Send>, filesytem_type: FileSystemType) -> LibUserResult<()> {
        let fat_type = match filesytem_type {
            FileSystemType::FAT12 => FatFsType::Fat12,
            FileSystemType::FAT16 => FatFsType::Fat16,
            FileSystemType::FAT32 => FatFsType::Fat32,
            _ => return Err(FileSystemError::UnsupportedOperation.into())
        };

        libfat::format_raw_partition(storage, fat_type).map_err(from_driver)
    }
}