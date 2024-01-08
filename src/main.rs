#![feature(int_roundings)]

use mft::{
    attribute::{x30::FileNamespace, FileAttributeFlags, MftAttributeContent, MftAttributeType},
    MftParser,
};
use ntfs::{attribute_value::NtfsAttributeValue, KnownNtfsFileRecordNumber::*, Ntfs, NtfsReadSeek};
use std::{
    backtrace::Backtrace,
    ffi::c_void,
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    mem::size_of,
    ops::Range,
    str::from_utf8,
    *,
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, GENERIC_READ, HANDLE},
        Storage::FileSystem::*,
        System::{
            Ioctl::{DISK_GEOMETRY, IOCTL_DISK_GET_DRIVE_GEOMETRY},
            IO::DeviceIoControl,
        },
    },
};

// Win32 only handles disk IO that is sector aligned and operates on whole sectors
struct DiskReader {
    handle: HANDLE,
    virtual_file_ptr: i64,
    read_buf_ptr: Option<i64>,
    read_buf: Vec<u8>,
    geometry: DISK_GEOMETRY,
}

impl DiskReader {
    fn new(handle: HANDLE) -> Result<Self, io::Error> {
        let mut geometry: DISK_GEOMETRY = Default::default();
        unsafe {
            assert_eq!(GetFileType(handle), FILE_TYPE_DISK);

            DeviceIoControl(
                handle,
                IOCTL_DISK_GET_DRIVE_GEOMETRY,
                None,
                0,
                Some(&mut geometry as *mut _ as *mut c_void),
                size_of::<DISK_GEOMETRY>() as u32,
                None,
                None,
            )?;
        }

        Ok(DiskReader {
            handle,
            virtual_file_ptr: 0,
            read_buf: vec![0u8; 2usize.pow(22)], // 4 MB buffer size
            read_buf_ptr: None,
            geometry,
        })
    }
}

impl Read for DiskReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        // println!("Read length: {}", buf.len());

        // Invalidate buffer if new read lands outside of it
        if let Some(p) = self.read_buf_ptr {
            if self.virtual_file_ptr < p {
                self.read_buf_ptr = None;
            }
            if self.virtual_file_ptr + buf.len() as i64 >= p + self.read_buf.len() as i64 {
                self.read_buf_ptr = None;
            }
        }

        match self.read_buf_ptr {
            Some(p) => {}
            None => unsafe {
                let bps = self.geometry.BytesPerSector as i64;
                // round down to lower sector
                let read_start = self.virtual_file_ptr / bps * bps;
                let read_len = self.read_buf.len();
                let read_bytes = read_start..read_start + read_len as i64;

                // println!(
                // "Buffered read: {} - {} [{}]",
                // read_bytes.start, read_bytes.end, read_len,
                // );

                SetFilePointerEx(self.handle, read_start, None, FILE_BEGIN)?;
                ReadFile(
                    self.handle,
                    Some(&mut self.read_buf[0..read_len]),
                    None,
                    None,
                )?;

                self.read_buf_ptr = Some(read_start);
            },
        }

        let vec_offs = (self.virtual_file_ptr - self.read_buf_ptr.unwrap()) as usize;
        buf.clone_from_slice(&self.read_buf[vec_offs..vec_offs + buf.len()]);
        self.virtual_file_ptr += buf.len() as i64;

        Ok(buf.len())
    }
}

impl Seek for DiskReader {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, io::Error> {
        let mut new_file_ptr = 0i64;
        unsafe {
            match pos {
                SeekFrom::Start(offset) => {
                    self.virtual_file_ptr = offset as i64;
                }
                SeekFrom::End(offset) => {
                    SetFilePointerEx(self.handle, 0, Some(&mut new_file_ptr), FILE_END)?;
                    self.virtual_file_ptr = new_file_ptr + offset;
                }
                SeekFrom::Current(offset) => {
                    self.virtual_file_ptr += offset;
                }
            };
        }

        Ok(self.virtual_file_ptr as u64)
    }
}

struct ReadSeekNtfsAttributeValue<'a, T>(&'a mut T, NtfsAttributeValue<'a, 'a>);

impl<T> Read for ReadSeekNtfsAttributeValue<'_, T>
where
    T: Read + Seek,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(self.1.read(&mut self.0, buf)?)
    }
}

impl<T> Seek for ReadSeekNtfsAttributeValue<'_, T>
where
    T: Read + Seek,
{
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        Ok(self.1.seek(&mut self.0, pos)?)
    }
}

fn main() -> Result<(), Box<dyn error::Error>> {
    unsafe {
        println!("Currently mounted logical drives (from GetLogicalDriveStringsW):");
        let mut buf = [0u16; 16384];
        let len = GetLogicalDriveStringsW(Some(&mut buf));
        let buf = buf
            .get(Range {
                start: 0,
                end: (len * 2) as usize,
            })
            .unwrap();
        for i in buf.split(|b| *b == 0u16) {
            if i.len() > 0 {
                println!("{}", String::from_utf16(i)?);
            }
        }

        let path: PCWSTR = w!(r"\\.\C:");

        let disk_handle = CreateFileW(
            path,
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )?;

        let mut disk = DiskReader::new(disk_handle)?;

        // Read 8 byte system ID, should be "NTFS    "
        let mut buf = vec![0u8; disk.geometry.BytesPerSector as usize];
        ReadFile(disk_handle, Some(&mut buf), None, None)?;

        println!("System ID: \"{}\"", from_utf8(&buf[3..11])?);

        let fs = Ntfs::new(&mut disk)?;
        let label = fs.volume_name(&mut disk).unwrap()?.name().to_string();

        println!("Volume label: {}", label?);

        let file = fs.file(&mut disk, MFT as u64)?;
        let data = file.data(&mut disk, "").unwrap()?;
        let data_attr = data.to_attribute()?;
        let mft_data_value = data_attr.value(&mut disk)?;

        println!("MFT size: {}", mft_data_value.len());

        let mut f = File::create("filenames.txt")?;

        let mut read_seek = ReadSeekNtfsAttributeValue(&mut disk, mft_data_value);
        let mut mft = MftParser::from_read_seek(&mut read_seek, None)?;
        let mut root = mft.get_entry(RootDirectory as u64)?;
        let mut num_entries = 0;
        for er in mft.iter_entries() {
            let e = er?;
            // Files with inode > 24 are ordinary files/directories
            if e.header.record_number > 24 {
                for a in e
                    .iter_attributes_matching(Some(vec![MftAttributeType::FileName]))
                    .filter_map(|attr| attr.ok())
                {
                    match a.data {
                        MftAttributeContent::AttrX30(a) => {
                            if a.parent.entry == RootDirectory as u64 {
                                if a.namespace == FileNamespace::Win32
                                    || a.namespace == FileNamespace::Win32AndDos
                                {
                                    f.write_all(format!("{}\n", a.name).as_bytes())?;
                                    println!("{} [{:#x}]", a.name, a.flags);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                num_entries += 1;
            }
        }

        println!("Entries in MFT: {}", num_entries);
    }

    Ok(())
}
