use std::ops::Range;

use windows::Win32::Storage::FileSystem::*;

static ASCII_UPPER: [char; 26] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S',
    'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
];

fn main() {
    unsafe {
        println!("Mounted logical drives (from GetLogicalDrives):");
        let mut drive_bitmap = GetLogicalDrives();
        let mut drive_num = 0;
        while drive_bitmap != 0 {
            if drive_bitmap & 1 != 0 {
                println!("{}:\\", ASCII_UPPER[drive_num]);
            }
            drive_bitmap >>= 1;
            drive_num += 1;
        }

        println!("Mounted logical drives (from GetLogicalDriveStringsW):");
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
    }
}
