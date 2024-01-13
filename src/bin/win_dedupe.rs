#![feature(int_roundings)]
#![feature(iter_collect_into)]
use cursive::event::Event;
use cursive::theme::{BorderStyle, Palette};
use cursive::traits::With;
use cursive::views::{Button, Dialog, DummyView, LinearLayout, SelectView, TextView};
use cursive::{Cursive, CursiveExt};

use std::{*};
use windows::Win32::Storage::FileSystem::GetLogicalDriveStringsW;

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

fn deduplicate_files_menu(_s: &mut Cursive) {}

fn explore_a_volume_menu(_s: &mut Cursive, _volume: &str) {}

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
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf16(f).unwrap())
        .collect()
}
