mod client;
mod model;
mod tray;
mod view;
use fs2::FileExt;
use futures::channel::mpsc;
use gpui_kit::*;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    thread,
    time::Duration,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let mut home = None;
    let mut hidden = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--home" => {
                home = Some(PathBuf::from(
                    arguments.next().ok_or("--home needs a directory")?,
                ))
            }
            "--hidden" => hidden = true,
            _ => return Err(format!("Unknown UI argument: {argument}").into()),
        }
    }
    let mut home = home.unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").expect("HOME or --home required"))
            .join(".local/state/monohero")
    });
    if !home.is_absolute() {
        home = std::env::current_dir()?.join(home);
    }
    fs::create_dir_all(&home)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(home.join("ui.lock"))?;
    if let Err(error) = lock.try_lock_exclusive() {
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error.into());
        }
        if !hidden {
            let mut stream = UnixStream::connect(home.join("ui.sock"))?;
            stream.write_all(b"show\n")?;
        }
        return Ok(());
    }
    let socket = home.join("ui.sock");
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let (sender, receiver) = mpsc::unbounded();
    let messages = sender.clone();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            if BufReader::new(&mut stream).read_line(&mut line).is_ok() && line.trim() == "show" {
                let _ = messages.unbounded_send(tray::UiEvent::Show);
            }
        }
    });
    let reopen = sender.clone();
    let application = application().with_assets(assets::Assets);
    application.on_reopen(move |_| {
        let _ = reopen.unbounded_send(tray::UiEvent::Show);
    });
    application.run(move |cx| {
        init(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1160.), px(800.)),
                cx,
            ))),
            window_min_size: Some(size(px(820.), px(500.))),
            show: !hidden,
            focus: !hidden,
            ..Default::default()
        };
        match open_window(options, cx, move |window, cx| {
            window.set_window_title("Monohero — Queues");
            let view =
                cx.new(|cx| view::Dashboard::new(home, hidden, sender, receiver, window, cx));
            let weak = view.downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                let _ = weak.update(cx, |view, cx| view.hide(cx));
                false
            });
            view
        }) {
            Ok(_) => {
                if !hidden {
                    cx.activate(true);
                }
            }
            Err(error) => {
                eprintln!("Cannot open Monohero queues: {error}");
                cx.quit();
            }
        }
    });
    fs::remove_file(socket)?;
    drop(lock);
    Ok(())
}
