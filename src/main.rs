#![feature(int_roundings)]
#![feature(iter_collect_into)]
mod win_dedupe;

use cursive::event::Event;
use cursive::theme::{BorderStyle, Palette};
use cursive::traits::With;
use cursive::views::{Button, Dialog, DummyView, LinearLayout, SelectView, TextView};
use cursive::{Cursive, CursiveExt};

use mft::attribute::header::ResidentialHeader::*;

use mft::{
    attribute::{MftAttributeContent, MftAttributeType},
    MftParser,
};

use ntfs::{KnownNtfsFileRecordNumber::*, Ntfs};
use std::collections::HashSet;

use std::ffi::CString;
use std::slice::Split;
use std::string::FromUtf16Error;
use std::{ops::Range, str::from_utf8, *};
use windows::Win32::Foundation::CloseHandle;
use windows::{
    core::{w, PCWSTR},
    Win32::{Foundation::GENERIC_READ, Storage::FileSystem::*},
};

use crate::win_dedupe::{DiskReader, FileMetadata, ReadSeekNtfsAttributeValue};

fn main() -> Result<(), Box<dyn error::Error>> {
    println!("{:#?}", get_logical_volumes());

    let mut siv = Cursive::new();

    // Start with a nicer theme than default
    siv.set_theme(cursive::theme::Theme {
        shadow: false,
        borders: BorderStyle::Simple,
        palette: Palette::retro().with(|palette| {
            use cursive::theme::BaseColor::*;
            {
                // First, override some colors from the base palette.
                use cursive::theme::Color::TerminalDefault;
                use cursive::theme::PaletteColor::*;

                palette[Background] = TerminalDefault;
                palette[View] = TerminalDefault;
                palette[Primary] = White.dark();
                palette[TitlePrimary] = Blue.light();
                palette[Secondary] = Blue.light();
                palette[Highlight] = Blue.dark();
            }

            {
                // Then override some styles.
                use cursive::theme::Effect::*;
                use cursive::theme::PaletteStyle::*;
                use cursive::theme::Style;
                palette[Highlight] = Style::from(Blue.light()).combine(Bold).combine(Reverse);
                palette[EditableTextCursor] = Style::secondary().combine(Reverse).combine(Underline)
            }
        }),
    });

    let buttons = LinearLayout::vertical()
        .child(TextView::new(
"WinDedupe is an application for finding and removing duplicate files on Windows machines.

WinDedupe accelerates search by reading the Master File Table of NTFS-formatted volumes.
Finding duplicate files on other filesystems is slower.

Select an option:"
    ))
        .child(Button::new("Find duplicate files", deduplicate_files_menu))
        .child(Button::new("Explore volumes", explore_volumes_menu))
        .child(DummyView)
        .child(Button::new("Quit", Cursive::quit));

    siv.add_layer(Dialog::around(buttons).title("Welcome to WinDedupe!"));

    siv.add_global_callback(Event::CtrlChar('c'), Cursive::quit);

    siv.run();

    Ok(())
}

fn deduplicate_files_menu(s: &mut Cursive) {}

fn explore_a_volume_menu(s: &mut Cursive, volume: &str) {}

fn explore_volumes_menu(s: &mut Cursive) {
    let mut select = SelectView::<String>::new().on_submit(explore_a_volume_menu);

    select.add_all_str(get_logical_volumes());
    println!("{:#?}", get_logical_volumes());

    s.pop_layer();
    s.add_layer(Dialog::around(select).title("Select a Volume"));
}

fn get_logical_volumes() -> Vec<String> {
    let mut buf;
    unsafe {
        buf = vec![0u16; GetLogicalDriveStringsW(None) as usize];
        GetLogicalDriveStringsW(Some(&mut buf));
    }

    // split buffer by nulls
    buf.split(|b| *b == 0u16)
        .filter(|f| f.len() > 0)
        .map(|f| String::from_utf16(f).unwrap())
        .collect()
}

fn mft_list_dir_test() -> Result<(), Box<dyn error::Error>> {
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

    Ok(())
}
