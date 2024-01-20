#![feature(int_roundings)]
#![feature(iter_collect_into)]
#![feature(try_blocks)]

use cursive::event::Event;
use cursive::theme::{BorderStyle, Palette};
use cursive::traits::With;
use cursive::views::{Button, Dialog, DummyView, LinearLayout, ProgressBar, ScrollView, SelectView, TextView};
use cursive::{Cursive, CursiveExt};

use clap::Parser;

use std::*;
use std::cmp::Ordering;
use cursive::utils::Counter;
use ntfs::KnownNtfsFileRecordNumber::RootDirectory;
use num_format::{Locale, ToFormattedString};
use win_dedupe::{get_mft_entry_count, VolumeIndexFlatArray, VolumeIndexTree, VolumeReader};
use anyhow::Result;

use winsafe::{GetLogicalDriveStrings, GetVolumeInformation};

/// Search for a pattern in a file and display the lines that contain it.
#[derive(Parser)]
struct Cli {
    path: Option<String>,
}

#[derive(Default)]
struct UserData {
    index: Option<VolumeIndexTree>,
    dir_stack: Vec<usize>,
    drive_letter: char,
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

    let args = Cli::parse();
    if let Some(path) = args.path {
        explore_a_volume_loading(&mut siv, &path);
    } else {
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
    }

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

    get_user_data(s).drive_letter = drive_letter;

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

    get_user_data(s).index = Some(index.build_tree());
    s.cb_sink().send(Box::new(finished_loading)).unwrap();
}

fn get_user_data(s: &mut Cursive) -> &mut UserData {
    s.user_data::<UserData>().unwrap()
}

fn finished_loading(s: &mut Cursive) {
    s.set_autorefresh(false);
    get_user_data(s).dir_stack.push(RootDirectory as usize);
    explore_a_volume_screen(s);
}

fn explore_a_volume_screen(s: &mut Cursive) {
    let mut select = SelectView::<(usize, bool)>::new()
        .on_submit(|s, (inode, push_to_stack) | {
            let user_data = get_user_data(s);
            let index = user_data.index.as_ref().unwrap();

            if index.0[*inode].as_ref().unwrap().is_dir {
                if *push_to_stack {
                    user_data.dir_stack.push(*inode);
                } else {
                    user_data.dir_stack.pop();
                }
                explore_a_volume_screen(s);
            }
        });

    let u = get_user_data(s);
    let parent_inode = u.dir_stack.last().unwrap();
    let index = u.index.as_ref().unwrap();

    for i in index.dir_children(*parent_inode).unwrap() {
        let i = *i as usize;
        let f = index.0[i].as_ref().unwrap();
        if f.is_dir {
            select.add_item(format!("{}/", f.name.as_ref().unwrap()), (i, true));
        } else {
            select.add_item(f.name.as_ref().unwrap(), (i, true));
        }
    }

    select.sort_by(|(inode0, _), (inode1, _)| {
        let file0 = u.index.as_ref().unwrap().0[*inode0].as_ref().unwrap();
        let file1 = u.index.as_ref().unwrap().0[*inode1].as_ref().unwrap();

        if file0.is_dir && !file1.is_dir { return Ordering::Less }
        if !file0.is_dir && file1.is_dir { return Ordering::Greater }

        file0.name.as_ref().unwrap().to_lowercase().cmp(&file1.name.as_ref().unwrap().to_lowercase())
    });

    if let Some(i) = u.dir_stack.iter().rev().nth(1) {
        select.insert_item(0, "../", (*i, false));
    }

    let mut title = format!("Explore: {}:/", u.drive_letter.to_uppercase());
    if let Some((_, tail)) = u.dir_stack.split_first() {
        for inode in tail {
            title.push_str(&*format!("{}/", index.0[*inode].as_ref().unwrap().name.as_ref().unwrap()));
        }
    }

    s.pop_layer();
    s.add_layer(
        Dialog::around(ScrollView::new(select))
            .title(title)
    )
}