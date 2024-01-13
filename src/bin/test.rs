use std::collections::HashSet;
use std::error;
use std::str::from_utf8;
use mft::attribute::{MftAttributeContent, MftAttributeType};
use mft::attribute::header::ResidentialHeader::{NonResident, Resident};
use mft::MftParser;
use ntfs::KnownNtfsFileRecordNumber::MFT;
use ntfs::Ntfs;
use windows::core::{PCWSTR, w};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile};
use win_dedupe::{DiskReader, FileMetadata, ReadSeekNtfsAttributeValue};

fn main() -> Result<(), Box<dyn error::Error>> {
    let path: PCWSTR = w!(r"\\.\C:");
    let mut file_metadata: Vec<Option<FileMetadata>>;

    unsafe {
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

    Ok(())
}
