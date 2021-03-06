pub mod scan;
pub mod traits;

mod error;
pub use error::MemHackError;

use std::mem::size_of;
use std::path::Path;
use std::ptr;

use traits::*;

use windows::Win32::Foundation::CHAR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HINSTANCE, PSTR};
use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32,
};
use windows::Win32::System::ProcessStatus::{K32EnumProcessModules, K32GetModuleFileNameExA};
use windows::Win32::System::Threading::OpenProcess;

#[allow(unused_imports)]
use windows::Win32::Foundation::GetLastError;

const PROCESS_ACCESS_RIGHTS: u32 = 0x10 | 0x20 | 0x8 | 0x400; //PROCESS_VM_READ || PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_INFORMATION

pub struct MemHook {
    handle: HANDLE,
}

impl MemHook {
    //---------- Initialization Functions ----------
    pub fn from_pid(pid: u32) -> Result<Self, MemHackError> {
        let handle: HANDLE;
        unsafe {
            handle = OpenProcess(PROCESS_ACCESS_RIGHTS.into(), false, pid);
        }

        if handle.0 == 0 {
            Err(MemHackError::CreateHookError)
        } else {
            Ok(MemHook { handle })
        }
    }

    pub fn from_process(process_name: &str) -> Result<Self, MemHackError> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(0x2.into(), 0);

            let mut process = PROCESSENTRY32 {
                dwSize: size_of::<PROCESSENTRY32>() as u32,
                ..Default::default()
            };

            if Process32First(snapshot, &mut process).as_bool() {
                loop {
                    let entry_name_bytes = process
                        .szExeFile
                        .iter()
                        .filter(|c| c.0 != 0)
                        .map(|c| c.0)
                        .collect::<Vec<u8>>();

                    let entry_name = std::str::from_utf8(&entry_name_bytes).unwrap();

                    if entry_name == process_name {
                        CloseHandle(snapshot);
                        return Self::from_pid(process.th32ProcessID);
                    }

                    process.szExeFile = [CHAR(0); 260];

                    if !Process32Next(snapshot, &mut process).as_bool() {
                        CloseHandle(snapshot);
                        return Err(MemHackError::CreateHookError);
                    }
                }
            }
            CloseHandle(snapshot);
        }
        Err(MemHackError::CreateHookError)
    }

    pub fn get_module_base_address(&self, module: &str) -> Result<usize, MemHackError> {
        let modules = &mut [HINSTANCE::default(); 1024];
        let mut bytes_needed = 0u32;

        unsafe {
            if K32EnumProcessModules(
                self.handle,
                modules as *mut _,
                (size_of::<HINSTANCE>() * modules.len()) as u32,
                &mut bytes_needed as *mut _,
            )
            .as_bool()
            {
                for i in 0..(bytes_needed / size_of::<HINSTANCE>() as u32) {
                    let mut mod_name = [0u8; 1024];

                    let str_len = K32GetModuleFileNameExA(
                        self.handle,
                        modules[i as usize],
                        PSTR(&mut mod_name as *mut _),
                        mod_name.len() as _,
                    );

                    let str_mod_name = std::str::from_utf8(&mod_name[..(str_len as usize)]);
                    if let Err(_) = str_mod_name {
                        continue;
                    }

                    let path = Path::new(str_mod_name.unwrap());
                    let filename = path.file_name();

                    if let Some(f) = filename {
                        if f == module {
                            return Ok(modules[i as usize].0 as _);
                        }
                    }
                }
            }
        }
        Err(MemHackError::ModuleNotFound(module.to_string()))
    }

    //---------- Memory Reading Functions ---------->

    pub fn get_pointer_address(&self, mut base: usize, offsets: &[usize]) -> Result<usize, MemHackError> {
        if offsets.is_empty() {
            return Ok(base);
        }

        for offset in offsets.iter().take(offsets.len() - 1) {
            base = self.read_val(base + offset)?;
        }
        Ok(base + offsets.get(offsets.len() - 1).unwrap())
    }

    pub fn read_bytes_const<const N: usize>(&self, address: usize) -> Result<[u8; N], MemHackError> {
        let mut buffer = [0u8; N];
        unsafe {
            let successful = ReadProcessMemory(
                self.handle,
                address as *const _,
                buffer.as_mut_ptr() as *mut _,
                N,
                ptr::null_mut(),
            );

            if !successful.as_bool() {
                return Err(MemHackError::MemoryReadError {address, bytes_to_read: N});
            }
        }
        Ok(buffer)
    }

    pub fn read_bytes_const_ptr<const N: usize>(
        &self,
        base: usize,
        offsets: &[usize],
    ) -> Result<[u8; N], MemHackError> {
        let address = self.get_pointer_address(base, offsets)?;
        self.read_bytes_const(address)
    }

    pub fn read_bytes(&self, address: usize, bytes_to_read: usize) -> Result<Vec<u8>, MemHackError> {
        let mut buffer: Vec<u8> = Vec::with_capacity(bytes_to_read);
        unsafe {
            buffer.set_len(bytes_to_read);
            let successful = ReadProcessMemory(
                self.handle,
                address as *const _,
                buffer.as_mut_ptr() as *mut _,
                bytes_to_read,
                ptr::null_mut(),
            );

            if !successful.as_bool() {
                return Err(MemHackError::MemoryReadError {address, bytes_to_read});
            }
        }
        Ok(buffer)
    }

    pub fn read_bytes_ptr(
        &self,
        base: usize,
        offsets: &[usize],
        bytes_to_read: usize,
    ) -> Result<Vec<u8>, MemHackError> {
        let address = self.get_pointer_address(base, offsets)?;
        self.read_bytes(address, bytes_to_read)
    }

    pub fn read_val<T: FromBytes>(&self, address: usize) -> Result<T, MemHackError> {
        let bytes = self.read_bytes(address, size_of::<T>())?;
        match T::from_bytes(&bytes) {
            Some(value) => Ok(value),
            None => Err(MemHackError::MemoryReadError {address, bytes_to_read: size_of::<T>()})
        }
    }

    pub fn read_val_ptr<T: FromBytes>(&self, base: usize, offsets: &[usize]) -> Result<T, MemHackError> {
        let address = self.get_pointer_address(base, offsets)?;
        let bytes = self.read_bytes(address, size_of::<T>())?;
        match T::from_bytes(&bytes) {
            Some(value) => Ok(value),
            None => Err(MemHackError::MemoryReadError {address, bytes_to_read: size_of::<T>()})
        }
    }

    //---------- Memory Writing Functions ---------->

    pub fn write_bytes(&self, address: usize, bytes_to_write: &[u8]) -> Result<(), MemHackError> {
        let mut bytes_written: usize = 0;
        let successful = unsafe {
            WriteProcessMemory(
                self.handle,
                address as *mut _,
                bytes_to_write.as_ptr() as *const _,
                bytes_to_write.len(),
                &mut bytes_written as *mut _,
            )
        };

        if successful.as_bool() {
            return Ok(());
        }
        Err(MemHackError::MemoryWriteError {address, bytes_written})
    }

    pub fn write_bytes_ptr(
        &self,
        base: usize,
        offsets: &[usize],
        bytes_to_write: &[u8],
    ) -> Result<(), MemHackError> {
        let address = self.get_pointer_address(base, offsets)?;
        self.write_bytes(address, bytes_to_write)
    }

    pub fn write_val<T: ToBytes>(&self, address: usize, value: T) -> Result<(), MemHackError> {
        let bytes = value.to_bytes();
        self.write_bytes(address, &bytes)
    }

    pub fn write_val_ptr<T: ToBytes>(
        &self,
        base: usize,
        offsets: &[usize],
        value: T,
    ) -> Result<(), MemHackError> {
        let bytes = value.to_bytes();
        self.write_bytes_ptr(base, offsets, &bytes)
    }

    //---------- Other Functions ---------->

    pub fn close(self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
