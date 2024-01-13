use std::error;
use ntfs::KnownNtfsFileRecordNumber::{MFT, RootDirectory, Volume};
use win_dedupe::{VolumeReader, FileMetadata, ReadSeekNtfsAttributeValue, VolumeIndexFlatArray};

fn main() -> Result<(), Box<dyn error::Error>> {
    let path = r"\\.\C:";
    println!("Opening raw volume: \"{}\"...", path);
    let mut reader = VolumeReader::open_path(path)?;
    println!("Reading file metadata from MFT and building index...");
    let index = VolumeIndexFlatArray::from(&mut reader)?;
    println!("Building tree...");
    let index = index.build_tree();

    let list_dir = |inode: usize| {
        if let Some(file) = &index.0[inode] {
            for i in &file.children_indices {
                // Files with inode > 24 are ordinary files/directories
                let child = index.0[*i as usize].as_ref().unwrap();
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

    list_dir(RootDirectory as usize);

    Ok(())
}
