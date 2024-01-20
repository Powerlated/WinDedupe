use std::error;
use ntfs::KnownNtfsFileRecordNumber::{RootDirectory};
use win_dedupe::{VolumeReader, VolumeIndexFlatArray};

fn main() -> Result<(), Box<dyn error::Error>> {
    let path = r"\\.\C:";
    println!("Opening raw volume: \"{}\"...", path);
    let mut reader = VolumeReader::open_path(path)?;
    println!("Reading file metadata from MFT and building index...");
    let index = VolumeIndexFlatArray::from(&mut reader, None)?;
    println!("Building tree...");
    let index = index.build_tree();

    if let Some(children) = index.dir_children(RootDirectory as usize) {
        println!("Root directory has children");
        for c in children {
            if let Some(f) = &index.0[*c as usize] {
                println!("{}", f.name.as_ref().unwrap());
            }
        }
    }

    Ok(())
}
