//! h120-play — .h120 stream player: GTK4 + libadwaita window, 25 fps.
//!
//! A separate binary from the `h120` CLI so the latter stays free of any
//! graphical dependency. Decoding runs on a dedicated thread; frames
//! converted to RGB arrive through a bounded channel (natural
//! back-pressure), and a 40 ms GLib tick displays them via a
//! `gdk::MemoryTexture`.

use adw::prelude::*;
use anyhow::{Context, Result};
use clap::Parser;
use gtk::{gdk, glib};
use gtk4 as gtk;
use h120::codec::decoder::Decoder;
use h120::source;
use h120::y4m::Frame444;
use libadwaita as adw;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

const FRAME_INTERVAL_MS: u64 = 40; // 25 frames/s

struct RgbFrame {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
    index: u64,
    total_pass: u64,
}

/// YCbCr (BT.601, limited range) → interleaved RGB conversion.
fn to_rgb(f: &Frame444) -> Vec<u8> {
    let mut rgb = vec![0u8; f.w * f.h * 3];
    for i in 0..f.w * f.h {
        let y = (f.y[i] as i32 - 16) * 298;
        let cb = f.cb[i] as i32 - 128;
        let cr = f.cr[i] as i32 - 128;
        let r = (y + 409 * cr + 128) >> 8;
        let g = (y - 100 * cb - 208 * cr + 128) >> 8;
        let b = (y + 516 * cb + 128) >> 8;
        rgb[i * 3] = r.clamp(0, 255) as u8;
        rgb[i * 3 + 1] = g.clamp(0, 255) as u8;
        rgb[i * 3 + 2] = b.clamp(0, 255) as u8;
    }
    rgb
}

/// Decoding thread: sends frames in a loop as long as the window lives.
fn decoder_thread(data: Vec<u8>, tx: mpsc::SyncSender<RgbFrame>, looping: Arc<AtomicBool>) {
    let mut pass = 0u64;
    loop {
        let mut dec = Decoder::new(&data);
        let mut index = 0u64;
        loop {
            let fields = match dec.next_frame() {
                Ok(Some(fields)) => fields,
                Ok(None) => break,
                Err(e) => {
                    eprintln!("decoding error: {e}");
                    break;
                }
            };
            let frame = source::egress(&fields);
            let rgb = to_rgb(&frame);
            if tx
                .send(RgbFrame { w: frame.w, h: frame.h, rgb, index, total_pass: pass })
                .is_err()
            {
                return; // window closed
            }
            index += 1;
        }
        if index == 0 || !looping.load(Ordering::Relaxed) {
            return;
        }
        pass += 1;
    }
}

#[derive(Parser)]
#[command(
    name = "h120-play",
    version,
    about = "Graphical player (GTK4 + libadwaita) for H.120 video streams"
)]
struct Cli {
    /// .h120 file to play
    input: std::path::PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(&cli.input)
}

fn run(input: &Path) -> Result<()> {
    let data =
        std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let title = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "H.120 stream".into());

    let app = adw::Application::builder()
        .application_id("fr.sofianelasri.h120-player")
        .build();

    app.connect_activate(move |app| {
        build_ui(app, data.clone(), title.clone());
    });
    // No arguments: clap has already consumed them.
    app.run_with_args::<&str>(&[]);
    Ok(())
}

fn build_ui(app: &adw::Application, data: Vec<u8>, title: String) {
    let looping = Arc::new(AtomicBool::new(true));
    let (tx, rx) = mpsc::sync_channel::<RgbFrame>(4);
    {
        let looping = looping.clone();
        std::thread::spawn(move || decoder_thread(data, tx, looping));
    }

    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Fill)
        .hexpand(true)
        .vexpand(true)
        .build();
    // The H.120 image (256×286 + 2 padding lines) covers a 4:3 screen: the
    // pixels are not square, so the AspectFrame enforces the correct ratio.
    let aspect = gtk::AspectFrame::builder()
        .ratio(4.0 / 3.0)
        .obey_child(false)
        .hexpand(true)
        .vexpand(true)
        .build();
    aspect.set_child(Some(&picture));

    let window_title = adw::WindowTitle::new(&title, "H.120 — 625 lines / 50 fields / 25 fps");

    let pause_btn = gtk::ToggleButton::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Pause (space)")
        .build();

    let loop_btn = gtk::ToggleButton::builder()
        .icon_name("media-playlist-repeat-symbolic")
        .tooltip_text("Loop playback")
        .active(true)
        .build();
    {
        let looping = looping.clone();
        loop_btn.connect_toggled(move |b| looping.store(b.is_active(), Ordering::Relaxed));
    }

    let header = adw::HeaderBar::builder().title_widget(&window_title).build();
    header.pack_start(&pause_btn);
    header.pack_end(&loop_btn);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&aspect));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .default_width(770)
        .default_height(625)
        .content(&toolbar)
        .build();

    // Space = pause.
    let key = gtk::EventControllerKey::new();
    {
        let pause_btn = pause_btn.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::space {
                pause_btn.set_active(!pause_btn.is_active());
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    window.add_controller(key);

    let rx = Rc::new(RefCell::new(rx));
    let tick = {
        let picture = picture.clone();
        let window_title = window_title.clone();
        let pause_btn = pause_btn.clone();
        move || {
            if pause_btn.is_active() {
                return glib::ControlFlow::Continue;
            }
            match rx.borrow().try_recv() {
                Ok(frame) => {
                    let bytes = glib::Bytes::from_owned(frame.rgb);
                    let texture = gdk::MemoryTexture::new(
                        frame.w as i32,
                        frame.h as i32,
                        gdk::MemoryFormat::R8g8b8,
                        &bytes,
                        frame.w * 3,
                    );
                    picture.set_paintable(Some(&texture));
                    let secs = frame.index as f64 / 25.0;
                    let pass = if frame.total_pass > 0 {
                        format!(" — loop {}", frame.total_pass + 1)
                    } else {
                        String::new()
                    };
                    window_title.set_subtitle(&format!(
                        "frame {} · {:02}:{:05.2}{}",
                        frame.index,
                        (secs / 60.0) as u32,
                        secs % 60.0,
                        pass
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    window_title.set_subtitle("end of stream");
                    return glib::ControlFlow::Break;
                }
            }
            glib::ControlFlow::Continue
        }
    };
    glib::timeout_add_local(std::time::Duration::from_millis(FRAME_INTERVAL_MS), tick);

    window.present();
}
