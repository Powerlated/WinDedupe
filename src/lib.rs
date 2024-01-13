use ntfs::{attribute_value::NtfsAttributeValue, Ntfs, NtfsReadSeek};
use std::collections::HashSet;
use std::{
    ffi::c_void,
    io::{Read, Seek, SeekFrom},
    mem::size_of,
    *,
};
use std::error::Error;
use std::io::SeekFrom::Start;
use std::str::from_utf8;
use mft::attribute::{MftAttributeContent, MftAttributeType};
use mft::attribute::header::ResidentialHeader::{NonResident, Resident};
use mft::MftParser;
use ntfs::KnownNtfsFileRecordNumber::MFT;
use windows::core::{PCWSTR, w};
use windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::*,
    System::{
        Ioctl::{DISK_GEOMETRY, IOCTL_DISK_GET_DRIVE_GEOMETRY},
        IO::DeviceIoControl,
    },
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};

// An struct storing the bare minimum needed for this program to work
#[derive(Clone)]
pub struct FileMetadata {
    pub name: Option<String>,
    pub index: u64,
    // Because hard links exist, a file can have multiple parent directories
    pub parent_indices: HashSet<u64>,
    pub is_dir: bool,
    pub file_size: u64,
    pub allocated_size: u64,
    pub children_indices: HashSet<u64>,
}

// Win32 only handles disk IO that is sector aligned and operates on whole sectors
pub struct VolumeReader {
    pub handle: HANDLE,
    pub virtual_file_ptr: i64,
    pub read_buf_ptr: Option<i64>,
    pub read_buf: Vec<u8>,
    pub geometry: DISK_GEOMETRY,
}

impl VolumeReader {
    pub fn from_raw_handle(handle: HANDLE) -> Result<Self, io::Error> {
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

        Ok(VolumeReader {
            handle,
            virtual_file_ptr: 0,
            read_buf: vec![0u8; 2usize.pow(22)], // 4 MB buffer size
            read_buf_ptr: None,
            geometry,
        })
    }

    pub fn open_path(path: &str) -> Result<VolumeReader, Box<dyn Error>> {
        let mut path: Vec<u16> = path.encode_utf16().collect();
        path.push(0);

        unsafe {
            let disk_handle = CreateFileW(
                PCWSTR::from_raw(path.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )?;

            Ok(VolumeReader::from_raw_handle(disk_handle)?)
        }
    }
}

impl Read for VolumeReader {
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

impl Seek for VolumeReader {
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

impl Drop for VolumeReader {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle).unwrap();
        }
    }
}

pub struct ReadSeekNtfsAttributeValue<'a, T>(pub &'a mut T, pub NtfsAttributeValue<'a, 'a>);

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

pub struct VolumeIndexFlatArray(pub Vec<Option<FileMetadata>>);

impl VolumeIndexFlatArray {
    pub fn from(reader: &mut VolumeReader) -> Result<Self, Box<dyn Error>> {
        let mut file_metadata: Vec<Option<FileMetadata>>;

        unsafe {
            // Read 8 byte system ID, should be "NTFS    "
            let mut buf = [0u8; 8];
            reader.seek(Start(3))?;
            reader.read_exact(&mut buf)?;
            assert_eq!(from_utf8(&buf)?, "NTFS    ");

            let fs = Ntfs::new(reader)?;
            let label = fs.volume_name(reader).unwrap()?.name().to_string();

            // println!("Volume label: {}", label?);

            let file = fs.file(reader, MFT as u64)?;
            let data = file.data(reader, "").unwrap()?;
            let data_attr = data.to_attribute()?;
            let mft_data_value = data_attr.value(reader)?;

            // println!("MFT size: {}", mft_data_value.len());

            let mut read_seek = ReadSeekNtfsAttributeValue(reader, mft_data_value);
            let mut mft = MftParser::from_read_seek(&mut read_seek, None)?;
            file_metadata = vec![None::<FileMetadata>; mft.get_entry_count() as usize];

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

                file_metadata[index] = Some(FileMetadata {
                    name,
                    index: index as u64,
                    parent_indices,
                    is_dir,
                    file_size,
                    allocated_size,
                    children_indices,
                });
            }

        }

        Ok(VolumeIndexFlatArray(file_metadata))
    }

    pub fn build_tree(mut self) -> VolumeIndexTree {
        // Build tree by linking parent directories to their children
        for i in 0..self.0.len() {
            if self.0[i].is_some() {
                let fm = &self.0[i];
                for parent_index in fm.clone().unwrap().parent_indices.iter() {
                    self.0[*parent_index as usize]
                        .as_mut()
                        .unwrap()
                        .children_indices
                        .insert(i as u64);
                }
            }
        }

        VolumeIndexTree(self.0)
    }
}
pub struct VolumeIndexTree(pub Vec<Option<FileMetadata>>);
