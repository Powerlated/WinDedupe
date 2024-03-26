use std::error;
use std::fs::File;
use std::io::{Cursor, Read};
use ntfs::KnownNtfsFileRecordNumber::{RootDirectory};
use win_dedupe::{VolumeReader, VolumeIndexFlatArray};

fn main() -> Result<(), Box<dyn error::Error>> {
    // println!("Opening MFT dump: \"./c.MFT\"...");
    //
    // let mut file = File::open("./c.MFT")?;
    // let mut buf = vec![0u8; file.metadata()?.len() as usize];
    // file.read(&mut buf)?;
    // let mut reader = Cursor::new(buf);

    let path = r"\\.\C:";
    println!("Opening raw volume: \"{}\"...", path);
    let mut reader = VolumeReader::open_path(path)?;
    println!("Reading file metadata and building index...");
    let index = VolumeIndexFlatArray::from_volume_reader(&mut reader, None)?;
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
