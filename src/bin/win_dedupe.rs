#![feature(int_roundings)]
#![feature(iter_collect_into)]
#![feature(try_blocks)]

use cursive::event::{Event, EventResult, Key};
use cursive::theme::{BorderStyle, Palette};
use cursive::traits::With;
use cursive::views::{Button, Dialog, DummyView, LinearLayout, OnEventView, ProgressBar, ScrollView, SelectView, TextView};
use cursive::{Cursive, CursiveExt};


use std::*;
use std::path::Component::RootDir;
use std::thread::JoinHandle;
use crossterm::style::Stylize;
use cursive::utils::Counter;
use ntfs::KnownNtfsFileRecordNumber::RootDirectory;
use num_format::{Locale, ToFormattedString};
use win_dedupe::{get_mft_entry_count, VolumeIndexFlatArray, VolumeIndexTree, VolumeReader};
use anyhow::Result;

use winsafe::{GetLogicalDriveStrings, GetVolumeInformation};

#[derive(Default)]
struct UserData {
    index: Option<VolumeIndexTree>,
    dir_stack: Vec<usize>,
}

fn main() -> Result<()> {
    let mut siv = Cursive::new();
    siv.set_user_data(UserData::default());

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
                palette[HighlightText] = White.light();
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


fn explore_volumes_menu(s: &mut Cursive) {
    let mut select = SelectView::<String>::new().on_submit(explore_a_volume_loading);

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

fn explore_a_volume_loading(s: &mut Cursive, path: &str) {
    let drive_letter = path.chars().nth(0).unwrap();
    assert!(drive_letter.is_alphabetic());
    assert_eq!(path.chars().nth(1).unwrap(), ':');

    let path = format!(r"\\.\{}:", drive_letter);
    let mut reader = VolumeReader::open_path(&path).unwrap();
    let entry_count = get_mft_entry_count(&mut reader).unwrap();

    s.set_autorefresh(true);

    let cb = s.cb_sink().clone();
    let counter = Counter::new(0);

    s.pop_layer();
    s.add_layer(
        Dialog::around(
            LinearLayout::vertical()
                .child(TextView::new(format!("Loading metadata for {} files...", entry_count.to_formatted_string(&Locale::en))))
                .child(ProgressBar::new().range(0, entry_count as usize).with_value(counter.clone())),
        )
            .title("Please Wait"),
    );

    thread::spawn(move || {
        let index = VolumeIndexFlatArray::from(&mut reader, Some(counter.0)).unwrap();
        cb.send(Box::new(|s| build_tree_loading_screen(s, index))).unwrap();
    });
}

fn build_tree_loading_screen(s: &mut Cursive, index: VolumeIndexFlatArray) {
    s.pop_layer();
    s.add_layer(
        Dialog::text("Building tree...")
            .title("Please Wait"),
    );

    s.user_data::<UserData>().unwrap().index = Some(index.build_tree());
    s.cb_sink().send(Box::new(finished_loading)).unwrap();
}

fn finished_loading(s: &mut Cursive) {
    s.set_autorefresh(false);
    explore_a_volume_screen(s, RootDirectory as usize);
}

#[derive(Clone, Copy)]
struct VolumeExploreParams {
    inode: usize,
    parent_inode: Option<usize>,
}

fn explore_a_volume_screen(s: &mut Cursive, parent_inode: usize) {
    let mut select = SelectView::<(usize, Option<usize>)>::new()
        .on_submit(|s, (inode, parent_inode)| {
            let mut user_data = s.user_data::<UserData>();
            let user_data = user_data.as_mut().unwrap();
            let index = user_data.index.as_ref().unwrap();

            if index.0[*inode].as_ref().unwrap().is_dir {
                if let Some(parent_inode) = parent_inode {
                    user_data.dir_stack.push(*parent_inode);
                }
                explore_a_volume_screen(s, *inode);
            }
        });
    let user_data = s.user_data::<UserData>();
    let user_data = user_data.as_ref().unwrap();
    let index = user_data.index.as_ref().unwrap();

    // let select = OnEventView::new(select)
    //     .on_pre_event_inner(Event::Key(Key::Left), |select, _| {
    //         s.
    //     });

    if let Some(last) = user_data.dir_stack.last() {
        select.add_item("../", (*last, None));
    }

    for i in index.dir_children(parent_inode).unwrap() {
        let i = *i as usize;
        let f = index.0[i].as_ref().unwrap();
        if f.is_dir {
            select.add_item(format!("{}/", f.name.as_ref().unwrap()), (i, Some(parent_inode)));
        } else {
            select.add_item(f.name.as_ref().unwrap(), (i, Some(parent_inode)));
        }
    }

    s.pop_layer();
    s.add_layer(
        Dialog::around(ScrollView::new(select))
            .title("Explore")
    )
}