use ntfs::Ntfs;
use std::{
    ffi::c_void,
    io::{Read, Seek, SeekFrom},
    mem::size_of,
    ops::Range,
    str::from_utf8,
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{GENERIC_ACCESS_RIGHTS, GENERIC_ALL, GENERIC_READ, HANDLE},
        Storage::FileSystem::*,
        System::{
            Ioctl::{DISK_GEOMETRY, IOCTL_DISK_GET_DRIVE_GEOMETRY},
            IO::DeviceIoControl,
        },
    },
};

static ASCII_UPPER: [char; 26] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S',
    'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
];

struct DiskReadSeek {
    handle: HANDLE,
}

impl Read for DiskReadSeek {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        println!("Read length: {}", buf.len());
        let mut num_bytes_read = 0u32;
        unsafe {
            ReadFile(self.handle, Some(buf), Some(&mut num_bytes_read), None)?;
        }

        Ok(num_bytes_read as usize)
    }
}

impl Seek for DiskReadSeek {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, std::io::Error> {
        let mut new_file_ptr = 0i64;
        unsafe {
            match pos {
                SeekFrom::Start(offset) => SetFilePointerEx(
                    self.handle,
                    offset as i64,
                    Some(&mut new_file_ptr),
                    FILE_BEGIN,
                ),
                SeekFrom::End(offset) => {
                    SetFilePointerEx(self.handle, offset, Some(&mut new_file_ptr), FILE_END)
                }
                SeekFrom::Current(offset) => {
                    SetFilePointerEx(self.handle, offset, Some(&mut new_file_ptr), FILE_CURRENT)
                }
            }?;
        }

        Ok(new_file_ptr as u64)
    }
}

fn main() {
    unsafe {
        // println!("Mounted logical drives (from GetLogicalDrives):");
        // let mut drive_bitmap = GetLogicalDrives();
        // let mut drive_num = 0;
        // while drive_bitmap != 0 {
        //     if drive_bitmap & 1 != 0 {
        //         println!("{}:\\", ASCII_UPPER[drive_num]);
        //     }
        //     drive_bitmap >>= 1;
        //     drive_num += 1;
        // }

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
                println!("{}", String::from_utf16(i).unwrap());
            }
        }

        let path: PCWSTR = w!(r"\\.\C:");

        let handle = CreateFileW(
            path,
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .unwrap();

        match GetFileType(handle) {
            FILE_TYPE_CHAR => println!("File type: Character file"),
            FILE_TYPE_DISK => println!("File type: Disk file"),
            FILE_TYPE_PIPE => println!("File type: Socket/pipe"),
            FILE_TYPE_REMOTE => println!("File type: Unused"),
            _ => {}
        }

        let mut disk_geometry: DISK_GEOMETRY = Default::default();
        DeviceIoControl(
            handle,
            IOCTL_DISK_GET_DRIVE_GEOMETRY,
            None,
            0,
            Some(&mut disk_geometry as *mut _ as *mut c_void),
            size_of::<DISK_GEOMETRY>() as u32,
            None,
            None,
        )
        .unwrap();

        println!("Bytes per sector: {}", disk_geometry.BytesPerSector);

        // Go to beginning of disk

        // Read 8 byte system ID: "NTFS    "
        let mut buf = vec![0u8; disk_geometry.BytesPerSector as usize];
        ReadFile(handle, Some(&mut buf), None, None).unwrap();

        println!("System ID: \"{}\"", from_utf8(&buf[3..11]).unwrap());

        let ntfs = Ntfs::new(&mut DiskReadSeek { handle });
    }
}
