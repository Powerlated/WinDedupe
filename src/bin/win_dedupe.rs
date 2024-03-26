#![feature(int_roundings)]
#![feature(iter_collect_into)]
#![feature(try_blocks)]

use cursive::event::Event;
use cursive::theme::{BorderStyle, Palette};
use cursive::traits::With;
use cursive::views::{Button, Dialog, DummyView, LinearLayout, ProgressBar, ScrollView, SelectView, TextView};
use cursive::{Cursive, CursiveExt};

use clap::Parser;

use cursive::view::Resizable;

use std::*;
use std::cmp::Ordering;
use cursive::utils::Counter;
use ntfs::KnownNtfsFileRecordNumber::RootDirectory;
use num_format::{Locale, ToFormattedString};
use win_dedupe::{FileMetadata, get_mft_entry_count, VolumeIndexFlatArray, VolumeIndexTree, VolumeReader};
use anyhow::Result;
use cursive::view::Nameable;
use cursive_table_view::{TableView, TableViewItem};

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
                // palette[EditableTextCursor] = Style::secondary().combine(Reverse).combine(Underline)
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
        let index = VolumeIndexFlatArray::from_volume_reader(&mut reader, Some(counter.0)).unwrap();
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

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
enum ExploreVolumeColumn {
    Name,
    Size,
}

impl TableViewItem<ExploreVolumeColumn> for (FileMetadata, bool) {
    fn to_column(&self, column: ExploreVolumeColumn) -> String {
        let (f, pop_from_stack) = self;
        match column {
            ExploreVolumeColumn::Name => {
                if *pop_from_stack {
                    String::from("↩️ ../")
                } else {
                    let name = f.name.clone().unwrap();
                    if f.is_dir {
                        format!("📁 {name}/")
                    } else {
                        format!("📄 {name}")
                    }
                }
            }
            ExploreVolumeColumn::Size => self.0.file_size.to_string(),
        }
    }

    fn cmp(&self, other: &Self, column: ExploreVolumeColumn) -> Ordering
        where
            Self: Sized,
    {
        if self.1 { return Ordering::Less; }
        if other.1 { return Ordering::Greater; }

        match column {
            ExploreVolumeColumn::Name => {
                if self.0.is_dir && !other.0.is_dir { return Ordering::Less; }
                if !self.0.is_dir && other.0.is_dir { return Ordering::Greater; }

                self.0.name.as_ref().unwrap().to_lowercase().cmp(&other.0.name.as_ref().unwrap().to_lowercase())
            }
            ExploreVolumeColumn::Size => {
                other.0.file_size.cmp(&self.0.file_size)
            }
        }
    }
}

fn explore_a_volume_screen(s: &mut Cursive) {
    let mut table = TableView::<(FileMetadata, bool), ExploreVolumeColumn>::new()
        .column(ExploreVolumeColumn::Name, "Name", |c| {
            c.width_percent(80)
        })
        .column(ExploreVolumeColumn::Size, "Size", |c| {
            c.ordering(Ordering::Greater)
                .width_percent(20)
        });

    let u = get_user_data(s);
    let parent_inode = u.dir_stack.last().unwrap();
    let index = u.index.as_ref().unwrap();

    for i in index.dir_children(*parent_inode).unwrap() {
        let i = *i as usize;
        table.insert_item((index.0[i].clone().unwrap(), false));
    }

    if let Some(i) = u.dir_stack.iter().rev().nth(1) {
        table.insert_item_at(0, (index.0[*i].clone().unwrap(), true));
    }

    table.set_on_submit(|s, _row, index| {
        let (f, pop_from_stack) = s
            .call_on_name("table", |table: &mut TableView<(FileMetadata, bool), ExploreVolumeColumn>| {
                table.borrow_item(index).unwrap().clone()
            })
            .unwrap();

        let u = get_user_data(s);
        if pop_from_stack {
            u.dir_stack.pop();
        } else if f.is_dir {
            u.dir_stack.push(f.index as usize);
        }

        explore_a_volume_screen(s);
    });

    let mut title = format!("Explore: {}:/", u.drive_letter.to_uppercase());
    if let Some((_, tail)) = u.dir_stack.split_first() {
        for inode in tail {
            title.push_str(&*format!("{}/", index.0[*inode].as_ref().unwrap().name.as_ref().unwrap()));
        }
    }

    s.pop_layer();
    s.add_layer(
        Dialog::around(table.with_name("table").full_screen())
            .title(title)
    )
}