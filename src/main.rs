#![feature(int_roundings)]

use mft::attribute::header::ResidentialHeader::*;
use mft::attribute::x30::FileNamespace::DOS;
use mft::{
    attribute::{MftAttributeContent, MftAttributeType},
    MftParser,
};
use multimap::MultiMap;
use ntfs::{attribute_value::NtfsAttributeValue, KnownNtfsFileRecordNumber::*, Ntfs, NtfsReadSeek};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::{
    ffi::c_void,
    io::{Read, Seek, SeekFrom},
    mem::size_of,
    ops::Range,
    str::from_utf8,
    *,
};
use windows::Win32::Foundation::CloseHandle;
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{GENERIC_READ, HANDLE},
        Storage::FileSystem::*,
        System::{
            Ioctl::{DISK_GEOMETRY, IOCTL_DISK_GET_DRIVE_GEOMETRY},
            IO::DeviceIoControl,
        },
    },
};

// An struct storing the bare minimum needed for this program to work
#[derive(Clone)]
struct FileMetadata {
    name: Option<String>,
    index: u64,
    // Because hard links exist, a file can have multiple parent directories
    parent_indices: HashSet<u64>,
    is_dir: bool,
    file_size: u64,
    allocated_size: u64,
    children_indices: HashSet<u64>,
}

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

        // Invalidate buffer if new read exceeds its boundaries
        if let Some(p) = self.read_buf_ptr {
            if self.virtual_file_ptr < p {
                self.read_buf_ptr = None;
            }
            if self.virtual_file_ptr + buf.len() as i64 >= p + self.read_buf.len() as i64 {
                self.read_buf_ptr = None;
            }
        }

        match self.read_buf_ptr {
            Some(_) => {}
            None => unsafe {
                let bps = self.geometry.BytesPerSector as i64;
                // round down to lower sector
                let read_start = self.virtual_file_ptr / bps * bps;
                let read_len = self.read_buf.len();

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
    let path: PCWSTR = w!(r"\\.\C:");
    let mut file_metadata: Vec<Option<FileMetadata>>;

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
        assert_eq!(from_utf8(&buf[3..11])?, "NTFS    ");

        let fs = Ntfs::new(&mut disk)?;
        let label = fs.volume_name(&mut disk).unwrap()?.name().to_string();

        println!("Volume label: {}", label?);

        let file = fs.file(&mut disk, MFT as u64)?;
        let data = file.data(&mut disk, "").unwrap()?;
        let data_attr = data.to_attribute()?;
        let mft_data_value = data_attr.value(&mut disk)?;

        println!("MFT size: {}", mft_data_value.len());

        let mut read_seek = ReadSeekNtfsAttributeValue(&mut disk, mft_data_value);
        let mut mft = MftParser::from_read_seek(&mut read_seek, None)?;
        file_metadata = vec![None::<FileMetadata>; mft.get_entry_count() as usize];
        println!("File count: {}", mft.get_entry_count());
        // let mut filenames_txt = BufWriter::new(File::create("filenames.txt")?);
        println!("Loading file metadata...");
        for (index, er) in mft.iter_entries().enumerate() {
            let e = er?;

            // Files with inode > 24 are ordinary files/directories
            let mut name = None::<String>;
            let mut parent_indices = HashSet::new();
            let mut is_dir = false;
            let mut file_size = 0u64;
            let mut allocated_size = 0u64;
            let children_indices = HashSet::new();

            for a in e.iter_attributes().filter_map(|attr| attr.ok()) {
                // Filename (AttrX30) is always resident so we are fine here
                // If a file has hard links it has multiple filename attributes
                match a.data {
                    MftAttributeContent::AttrX30(a) => {
                        parent_indices.insert(a.parent.entry);
                        // filenames_txt.write_fmt(format_args!("i:{} p:{} {}\n", index, a.parent.entry, a.name)).expect("Unable to write data");
                        name = Some(a.name);
                        is_dir = e.is_dir();
                    }
                    _ => {}
                }

                // Data (AttrX80) can be non-resident if it is too big for the MFT entry
                match a.header.type_code {
                    MftAttributeType::DATA => {
                        match a.header.residential_header {
                            Resident(h) => {
                                file_size = h.data_size as u64;
                                allocated_size = h.data_size as u64;
                            }
                            NonResident(h) => {
                                // mft crate docs say that valid_data_length and allocated_length are invalid if vcn_first != 0
                                // assert_eq!(h.vnc_first, 0);
                                file_size = h.file_size;
                                // When a file is compressed, allocated_length is an even multiple of the compression unit size rather than the cluster size.
                                allocated_size = h.allocated_length;
                                // Compression unit size = 2^x clusters
                                // println!("Compression unit size (bytes): {}", 2u32.pow(h.unit_compression_size as u32) * fs.cluster_size());
                            }
                        }
                    }
                    _ => {}
                }
            }

            file_metadata[index as usize] = Some(FileMetadata {
                name,
                index: index as u64,
                parent_indices,
                is_dir,
                file_size,
                allocated_size,
                children_indices,
            });
        }

        CloseHandle(disk_handle)?;
    }

    println!("Building tree...");
    // Build tree by linking parent directories to their children
    for i in 0..file_metadata.len() {
        if file_metadata[i].is_some() {
            let fm = &file_metadata[i];
            for parent_index in fm.clone().unwrap().parent_indices.iter() {
                file_metadata[*parent_index as usize]
                    .as_mut()
                    .unwrap()
                    .children_indices
                    .insert(i as u64);
            }
        }
    }

    let list_dir = |index: u64| {
        if let Some(file) = &file_metadata[index as usize] {
            for i in &file.children_indices {
                // Files with inode > 24 are ordinary files/directories
                let child = file_metadata[*i as usize].as_ref().unwrap();
                if *i > 24 {
                    println!(
                        "i:{} {}{}",
                        child.index,
                        child.name.as_ref().unwrap(),
                        if child.is_dir { "/" } else { "" }
                    );
                }
            }
        }
    };

    list_dir(2807);

    println!("Entries in MFT: {}", file_metadata.len());

    loop {}

    Ok(())
}
