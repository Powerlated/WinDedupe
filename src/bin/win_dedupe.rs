#![feature(int_roundings)]
#![feature(iter_collect_into)]
#![feature(try_blocks)]

use cursive::event::Event;
use cursive::theme::{BorderStyle, Palette};
use cursive::traits::With;
use cursive::views::{
    Button, Dialog, DummyView, LinearLayout, ProgressBar, ScrollView, SelectView, TextView,
};
use cursive::{Cursive, CursiveExt};

use ntfs::KnownNtfsFileRecordNumber::Volume;
use std::*;
use num_format::{Locale, ToFormattedString};
use win_dedupe::{get_mft_entry_count, VolumeIndexFlatArray, VolumeReader};
use windows::Win32::Storage::FileSystem::{
    GetLogicalDriveStringsW, GetVolumeInformationByHandleW, GetVolumeInformationW,
};
use winsafe::{GetLogicalDriveStrings, GetVolumeInformation};

fn main() -> Result<(), Box<dyn error::Error>> {
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
                palette[Primary] = White.light();
                palette[TitlePrimary] = Blue.light();
                palette[Secondary] = Blue.light();
                palette[Highlight] = Blue.light();
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

    siv.add_layer(Dialog::around(ScrollView::new(buttons)).title("Welcome to WinDedupe!"));

    siv.add_global_callback(Event::CtrlChar('c'), Cursive::quit);

    siv.run();

    Ok(())
}

fn deduplicate_files_menu(_s: &mut Cursive) {}

fn explore_a_volume_menu(s: &mut Cursive, path: &str) {
    let drive_letter = path.chars().nth(0).unwrap();
    assert!(drive_letter.is_alphabetic());
    assert_eq!(path.chars().nth(1).unwrap(), ':');

    let path = format!(r"\\.\{}:", drive_letter);
    let mut reader = VolumeReader::open_path(&path).unwrap();
    let entry_count = get_mft_entry_count(&mut reader).unwrap();

    s.set_autorefresh(true);

    s.pop_layer();
    s.add_layer(
        Dialog::around(
            LinearLayout::vertical()
                .child(TextView::new(format!("Loading metadata for {} files...", entry_count.to_formatted_string(&Locale::en))))
                .child(ProgressBar::new().range(0, entry_count as usize).with_task(move |counter| {
                    let index = VolumeIndexFlatArray::from(&mut reader, Some(counter.0)).unwrap();
                })),
        )
            .title("Please Wait"),
    );
}

fn explore_volumes_menu(s: &mut Cursive) {
    let mut select = SelectView::<String>::new().on_submit(explore_a_volume_menu);

    for v in GetLogicalDriveStrings().unwrap() {
        let mut name = String::default();
        let mut fs_name = String::default();
        GetVolumeInformation(Some(&v), Some(&mut name), None, None, None, Some(&mut fs_name)).unwrap();
        if fs_name == "NTFS" {
            select.add_item(format!("{} - {} - {}", v, name, fs_name), v);
        } else {
            select.add_item(format!("{} - {} - {} - Not NTFS, cannot scan", v, name, fs_name), v);
        }
    }

    s.pop_layer();
    s.add_layer(Dialog::around(select).title("Select a Volume"));
}
