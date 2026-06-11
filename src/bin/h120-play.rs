//! h120-play — lecteur de flux .h120 : fenêtre GTK4 + libadwaita, 25 i/s.
//!
//! Binaire séparé du CLI `h120` pour que ce dernier reste exempt de toute
//! dépendance graphique. Le décodage tourne sur un thread dédié ; les images
//! converties en RGB arrivent par un canal borné (contre-pression
//! naturelle), et un tick GLib de 40 ms les affiche via une
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

const FRAME_INTERVAL_MS: u64 = 40; // 25 images/s

struct RgbFrame {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
    index: u64,
    total_pass: u64,
}

/// Conversion YCbCr (BT.601, plage limitée) → RGB entrelacé.
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

/// Thread de décodage : envoie les images en boucle tant que la fenêtre vit.
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
                    eprintln!("erreur de décodage : {e}");
                    break;
                }
            };
            let frame = source::egress(&fields);
            let rgb = to_rgb(&frame);
            if tx
                .send(RgbFrame { w: frame.w, h: frame.h, rgb, index, total_pass: pass })
                .is_err()
            {
                return; // fenêtre fermée
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
    about = "Lecteur graphique (GTK4 + libadwaita) de flux vidéo H.120"
)]
struct Cli {
    /// Fichier .h120 à lire
    input: std::path::PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(&cli.input)
}

fn run(input: &Path) -> Result<()> {
    let data =
        std::fs::read(input).with_context(|| format!("lecture de {}", input.display()))?;
    let title = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "flux H.120".into());

    let app = adw::Application::builder()
        .application_id("io.github.h120.Reference")
        .build();

    app.connect_activate(move |app| {
        build_ui(app, data.clone(), title.clone());
    });
    // Pas d'arguments : clap les a déjà consommés.
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
    // L'image H.120 (256×286 + 2 lignes de bourrage) couvre un écran 4:3 :
    // les pixels ne sont pas carrés, l'AspectFrame impose le bon rapport.
    let aspect = gtk::AspectFrame::builder()
        .ratio(4.0 / 3.0)
        .obey_child(false)
        .hexpand(true)
        .vexpand(true)
        .build();
    aspect.set_child(Some(&picture));

    let window_title = adw::WindowTitle::new(&title, "H.120 — 625 lignes / 50 champs / 25 i/s");

    let pause_btn = gtk::ToggleButton::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Pause (espace)")
        .build();

    let loop_btn = gtk::ToggleButton::builder()
        .icon_name("media-playlist-repeat-symbolic")
        .tooltip_text("Lecture en boucle")
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

    // Espace = pause.
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
                        format!(" — boucle {}", frame.total_pass + 1)
                    } else {
                        String::new()
                    };
                    window_title.set_subtitle(&format!(
                        "image {} · {:02}:{:05.2}{}",
                        frame.index,
                        (secs / 60.0) as u32,
                        secs % 60.0,
                        pass
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    window_title.set_subtitle("fin du flux");
                    return glib::ControlFlow::Break;
                }
            }
            glib::ControlFlow::Continue
        }
    };
    glib::timeout_add_local(std::time::Duration::from_millis(FRAME_INTERVAL_MS), tick);

    window.present();
}
