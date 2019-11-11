//! AHCI Disk

use alloc::sync::Arc;
use core::fmt::{self, Debug, Formatter};

use spin::Mutex;

use sunrise_libuser::error::{Error, AhciError};
use sunrise_libuser::ahci::IDisk as IDiskInterface;
use sunrise_libuser::futures::WorkQueue;
use sunrise_libuser::zero_box::ZeroBox;
use sunrise_libuser::ahci::Block;

use crate::hba::*;


/// An AHCI Disk
///
/// Manages an AHCI port, and provides functions to read and write sectors.
///
/// # Memory
///
/// A disk is responsible for all allocated memory in use by the port. When dropped, the port
/// is put to a stop, pointers to these regions are cleared from the hardware, and the regions
/// are eventually de-allocated.
///
/// # Lifetime
///
/// A Disk holds a reference to its `Port Control Registers`,
/// which is located in the root `HBA Memory Registers` mapping (the one found at `BAR5`).
///
/// As this root mapping will never be unmapped, the lifetime of this reference is `'static`.
pub struct Disk {

    // memory zones

    /// Pointer back to the corresponding Port Control Registers, found at `BAR5[100h]`-`BAR5[10FFh]`.
    pub(super) px:         &'static mut Px,
    /// The allocated Received FIS memory zone that the port uses.
    pub(super) rfis:       ZeroBox<ReceivedFis>,
    /// The allocated Command List memory zone that the port uses.
    pub(super) cmd_list:   ZeroBox<CmdHeaderArray>,
    /// An allocated Command Table for each implemented Command List slot.
    pub(super) cmd_tables: [Option<ZeroBox<CmdTable>>; 32],

    // info obtained by the IDENTIFY command

    /// Number of addressable sectors of this disk. Each sector is 512 octets.
    pub(super) sectors: u64,
    /// Indicates if the device supports 48 bit addresses.
    pub(super) supports_48_bit: bool,
}

impl Disk {
    /// Returns the number of addressable 512-octet sectors for this disk.
    #[inline(never)]
    fn sector_count(&self, ) -> Result<u64, Error> {
        Ok(self.sectors)
    }

    /// Reads sectors from disk.
    ///
    /// Reads `sector_count` sectors starting from `lba`.
    #[inline(never)]
    fn read_dma(&mut self, buffer: *mut u8, buffer_len: usize, lba: u64, sector_count: u64) -> Result<(), Error> {
        if ((buffer as usize) % 2) != 0 {
            return Err(AhciError::InvalidArg.into());
        }

        if (buffer_len as u64) < sector_count * 512 {
            return Err(AhciError::InvalidArg.into());
        }
        if lba.checked_add(sector_count).filter(|sum| *sum <= self.sectors).is_none() {
            return Err(AhciError::InvalidArg.into());
        }
        // todo: AHCI: Read CI and figure out which slot to use
        // body: For now AHCI driver is single-threaded and blocking,
        // body: which means that the first slot is always available for use.
        // body:
        // body: If we want to make a multi-threaded implementation,
        // body: we will have to implement some logic to choose the slot.
        let command_slot_index = 0;
        let step = if !self.supports_48_bit {
            256
        } else {
            65536
        };

        for sector_step in (0..sector_count).step_by(step) {
            unsafe {
                // safe: - we just mapped buffer, so it is valid memory,
                //         and buffer_len is its length
                //         otherwise mapping it would have failed.
                //       - buffer[0..buffer_len] falls in a single mapping,
                //         we just mapped it.
                //       - command_slot_index is 0, which is always implemented (spec),
                //         and we give the cmd_header and cmd_table of this index.
                //       - px is initialised.
                Px::read_dma(
                    buffer.add(sector_step as usize * 512),
                    buffer_len - sector_step as usize * 512,
                    lba + sector_step,
                    core::cmp::min(sector_count - sector_step, step as u64),
                    self.px,
                    &mut self.cmd_list.slots[command_slot_index],
                    self.cmd_tables[command_slot_index].as_mut().unwrap(),
                    command_slot_index,
                    self.supports_48_bit
                )?
            }
        }
        Ok(())
    }

    /// Writes sectors to disk.
    ///
    /// Writes `sector_count` sectors starting from `lba`.
    #[inline(never)]
    fn write_dma(&mut self, buffer: *const u8, buffer_len: usize, lba: u64, sector_count: u64) -> Result<(), Error> {
        if ((buffer as usize) % 2) != 0 {
            return Err(AhciError::InvalidArg.into());
        }

        if (buffer_len as u64) < sector_count * 512 {
            return Err(AhciError::InvalidArg.into());
        }
        if lba.checked_add(sector_count).filter(|sum| *sum <= self.sectors).is_none() {
            return Err(AhciError::InvalidArg.into());
        }
        let command_slot_index = 0;
        unsafe {
            // safe: - we just mapped buffer, so it is valid memory,
            //         and buffer_len is its length
            //         otherwise mapping it would have failed.
            //       - buffer[0..buffer_len] falls in a single mapping,
            //         we just mapped it.
            //       - command_slot_index is 0, which is always implemented (spec),
            //         and we give the cmd_header and cmd_table of this index.
            //       - px is initialised.
            Px::write_dma(
                buffer,
                buffer_len,
                lba,
                sector_count,
                self.px,
                &mut self.cmd_list.slots[command_slot_index],
                self.cmd_tables[command_slot_index].as_mut().unwrap(),
                command_slot_index,
                self.supports_48_bit
            )?
        }
        Ok(())
    }
}

impl Drop for Disk {
    /// Dropping a disk brings the port to a stop, and clears the pointers from the hardware.
    fn drop(&mut self) {
        self.px.stop();
        self.px.clear_addresses();
    }
}

impl Debug for Disk {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        f.debug_struct("Disk")
            .field("sectors", &self.sectors)
            .field("px", &self.px)
            .finish()
    }
}

/// Interface to a disk.
#[derive(Debug, Clone)]
pub struct IDisk(Arc<Mutex<Disk>>);

impl IDisk {
    /// Creates an IDisk from the wrapped [Disk].
    pub fn new(value: Arc<Mutex<Disk>>) -> Self {
        Self(value)
    }
}

impl IDiskInterface for IDisk {
    /// Returns the number of addressable 512-octet sectors for this disk.
    fn sector_count(&mut self, _work_queue: WorkQueue<'static>) -> Result<u64, Error> {
        Ok(self.0.lock().sectors)
    }

    /// Reads sectors from disk.
    ///
    /// Reads `sector_count` sectors starting from `lba`.
    ///
    /// # Error
    ///
    /// - InvalidArg:
    ///     - `mapping_size` does not reflect the passed handle's size, or mapping it failed,
    ///     - `lba`, `sector_count`, or `lba + sector_count` is higher than the number of
    ///        addressable sectors on this disk,
    ///     - `sector_count` == 0.
    /// - BufferTooScattered:
    ///     - The passed handle points to memory that is so physically scattered it overflows
    ///       the PRDT. This can only happen for read/writes of 1985 sectors or more.
    ///       You should consider retrying with a smaller `sector_count`.
    fn read_dma(&mut self, _manager: WorkQueue<'static>, address: u64, out_blocks: &mut [sunrise_libuser::ahci::Block]) -> Result<(), Error> {
        self.0.lock().read_dma(out_blocks.as_mut_ptr() as *mut u8, out_blocks.len() * core::mem::size_of::<Block>(), address, out_blocks.len() as u64)
    }

    /// Writes sectors to disk.
    ///
    /// Writes `sector_count` sectors starting from `lba`.
    ///
    /// # Error
    ///
    /// - InvalidArg:
    ///     - `mapping_size` does not reflect the passed handle's size, or mapping it failed,
    ///     - `lba`, `sector_count`, or `lba + sector_count` is higher than the number of
    ///        addressable sectors on this disk,
    ///     - `sector_count` == 0.
    /// - BufferTooScattered:
    ///     - The passed handle points to memory that is so physically scattered it overflows
    ///       the PRDT. This can only happen for read/writes of 1985 sectors or more.
    ///       You should consider retrying with a smaller `sector_count`.
    fn write_dma(&mut self, _manager: WorkQueue<'static>, address: u64, in_blocks: &[sunrise_libuser::ahci::Block]) -> Result<(), Error> {
        self.0.lock().write_dma(in_blocks.as_ptr() as *const u8, in_blocks.len() * core::mem::size_of::<Block>(), address, in_blocks.len() as u64)
    }
}