use ntfs::Ntfs;
use std::collections::{BTreeSet, VecDeque};
use std::{
    ffi::c_void,
    io::{Read, Seek, SeekFrom},
    mem::size_of,
    *,
};
use std::collections::btree_set::Iter;
use std::io::SeekFrom::Start;
use std::iter::FilterMap;
use anyhow::Result;
use std::str::from_utf8;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use mft::attribute::{MftAttributeContent, MftAttributeType};
use mft::attribute::header::ResidentialHeader::{NonResident, Resident};
use mft::attribute::x30::FileNamespace::DOS;
use mft::entry::EntryFlags;
use mft::MftParser;
use ntfs::KnownNtfsFileRecordNumber::{MFT, RootDirectory};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_BEGIN, FILE_END, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, GetFileType, OPEN_EXISTING, ReadFile, SetFilePointerEx};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{DISK_GEOMETRY, IOCTL_DISK_GET_DRIVE_GEOMETRY};


// An struct storing the bare minimum needed for this program to work
#[derive(Clone)]
pub struct FileMetadata {
    pub name: Option<String>,
    pub index: u64,
    // Because hard links exist, a file can have multiple parent directories
    pub parent_indices: BTreeSet<usize>,
    pub is_dir: bool,
    pub file_size: u64,
    pub allocated_size: u64,
    pub children_indices: BTreeSet<usize>,
    pub children_size: u64,
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
    pub fn from_raw_handle(handle: HANDLE) -> Result<Self> {
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

    pub fn open_path(path: &str) -> Result<VolumeReader> {
        let mut path: Vec<u16> = path.encode_utf16().collect();
        path.push(0);

        unsafe {
            let disk_handle = CreateFileW(
                PCWSTR::from_raw(path.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
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

fn verify_ntfs_system_id<T: Read + Seek>(reader: &mut T) -> bool {
    // Read 8 byte system ID, should be "NTFS    "
    let mut buf = [0u8; 8];
    reader.seek(Start(3)).unwrap();
    reader.read_exact(&mut buf).unwrap();
    from_utf8(&buf).unwrap() == "NTFS    "
}

pub struct VolumeIndexFlatArray(pub Vec<Option<FileMetadata>>);

impl VolumeIndexFlatArray {
    pub fn from_mft_reader<T: Read + Seek>(reader: &mut T, progress_counter: Option<Arc<AtomicUsize>>) -> VolumeIndexFlatArray {
        let mut file_metadata: Vec<Option<FileMetadata>>;

        let mut mft = MftParser::from_read_seek(reader, None).unwrap();

        let entry_count = mft.get_entry_count();
        file_metadata = vec![None::<FileMetadata>; entry_count as usize];

        for (index, er) in mft.iter_entries().enumerate() {
            if let Ok(e) = er {

                // Files with inode > 24 are ordinary files/directories
                let mut name = None::<String>;
                let mut parent_indices = BTreeSet::new();
                let is_dir = e.header.flags.contains(EntryFlags::INDEX_PRESENT);
                let mut file_size = 0u64;
                let mut allocated_size = 0u64;
                let children_indices = BTreeSet::new();

                for a in e.iter_attributes().filter_map(|attr| attr.ok()) {
                    // Filename (AttrX30) is always resident so we are fine here
                    // If a file has hard links it has multiple filename attributes
                    match a.data {
                        MftAttributeContent::AttrX30(a) => {
                            if a.namespace != DOS {
                                parent_indices.insert(a.parent.entry as usize);
                                name = Some(a.name);
                            }
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
                                    // assert_eq!(h.vnc_first, 0);A
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
                    children_size: 0,
                });
            }

            // Send progress update for every percentage
            if index % (entry_count / 100) as usize == 0 {
                if let Some(ref progress_counter) = progress_counter {
                    progress_counter.store(index, Ordering::Relaxed);
                }
            }
        }

        VolumeIndexFlatArray(file_metadata)
    }

    pub fn from_volume_reader(reader: &mut VolumeReader, progress_counter: Option<Arc<AtomicUsize>>) -> Result<Self> {
        assert!(verify_ntfs_system_id(reader));

        let fs = Ntfs::new(reader)?;
        let file = fs.file(reader, MFT as u64)?;
        let data = file.data(reader, "").unwrap()?;
        let data_attr = data.to_attribute()?;
        let mut mft_reader = data_attr.value(reader)?.attach(reader);

        Ok(Self::from_mft_reader(&mut mft_reader, progress_counter))
    }

    pub fn build_tree(mut self) -> VolumeIndexTree {
        let mut parent_isnt_dir_count = 0;
        // Build tree by linking parent directories to their children
        for i in 0..self.0.len() {
            if self.0[i].is_some() {
                let fm = &self.0[i].clone();
                for parent_index in fm.clone().unwrap().parent_indices {
                    let parent = self.0[parent_index as usize]
                        .as_ref()
                        .expect("File refers to nonexistent parent directory. Possible filesystem corruption detected, please run chkdsk on the drive.");

                    let file_name = fm.as_ref().unwrap().name.as_deref().unwrap_or("<no name>");
                    let file_inode = fm.as_ref().unwrap().index;

                    let parent_name = parent.name.as_deref().unwrap_or("<no name>");
                    let parent_inode = parent.index;

                    if parent.is_dir
                    {
                        self.0[parent_index as usize]
                            .as_mut()
                            .unwrap()
                            .children_indices
                            .insert(i);
                    } else {
//                         println!(
//                             "File parent isn't directory??????
// File inode: {}
// File name: {}
// Parent inode: {}
// Parent name: {}
// ",
//                             file_inode, file_name,
//                             parent_inode, parent_name);

                        parent_isnt_dir_count += 1;
                    }
                }
            }
        }

        if parent_isnt_dir_count > 0 {
            eprintln!("[WARN] {} files didn't have directories as parents", parent_isnt_dir_count);
        }

        // let mut reverse_stack = Vec::<usize>::new();
        // let mut queue = VecDeque::<usize>::new();
        // let mut traversed = vec![false; self.0.len()];
        //
        // reverse_stack.push(RootDirectory as usize);
        // queue.push_back(RootDirectory as usize);
        //
        // println!("Items in file list: {}", self.0.len());
        // println!("Items in initial queue: {}", queue.len());
        //
        // let mut n = 0;
        //
        // while !queue.is_empty() {
        //     let parent = queue.pop_front().unwrap();
        //     let children = &self.0[parent].as_ref().unwrap().children_indices;
        //
        //     for i in children {
        //         queue.push_back(*i);
        //         reverse_stack.push(*i);
        //         traversed[*i] = true;
        //     }
        // }
        //
        // println!("Traversal stack size: {}", reverse_stack.len());

        // Calculate
        VolumeIndexTree(self.0)
    }
}

pub struct VolumeIndexTree(pub Vec<Option<FileMetadata>>);

impl VolumeIndexTree {
    pub fn dir_children(&self, inode: usize) -> Option<FilterMap<Iter<usize>, fn(&usize) -> Option<&usize>>> {
        if let Some(file) = &self.0[inode] {
            // Files with inode > 24 are ordinary files/directories
            return Some(file.children_indices.iter().filter_map(|i| if *i > 24 { Some(i) } else { None }));
        }

        None
    }
}

pub fn get_mft_entry_count(reader: &mut VolumeReader) -> Result<u64> {
    let fs = Ntfs::new(reader)?;
    let file = fs.file(reader, MFT as u64)?;
    let data = file.data(reader, "").unwrap()?;
    let data_attr = data.to_attribute()?;
    let mft_data_value = data_attr.value(reader)?.attach(reader);
    let mft = MftParser::from_read_seek(mft_data_value, None)?;

    Ok(mft.get_entry_count())
}