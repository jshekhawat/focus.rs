use anyhow::{anyhow, Result};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::{xproto::*, Event};
use x11rb::rust_connection::RustConnection;


#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct WindowKey {
    platform_id: String, // XID like "xid:0x3a00007"
    app_id: String,      // exe path if we can resolve, else pid/name
    title: String,
}

#[derive(Debug, Clone)]
struct FocusEvent {
    key: Option<WindowKey>, // None => no active window we could resolve
    ts: Instant,
}

fn main() -> Result<()> {
    let (conn, screen_num) = RustConnection::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    // Intern atoms we need up-front.
    let atom_net_active_window = intern(&conn, b"_NET_ACTIVE_WINDOW")?;
    let atom_net_wm_name = intern(&conn, b"_NET_WM_NAME")?;
    let atom_wm_name = AtomEnum::WM_NAME.into();
    let atom_utf8_string = intern(&conn, b"UTF8_STRING")?;
    let atom_net_wm_pid = intern(&conn, b"_NET_WM_PID")?;

    // Subscribe to PropertyChange on the root window to get _NET_ACTIVE_WINDOW updates.
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;
    conn.flush()?;

    // Debounce to avoid flicker (e.g., transient focus changes).
    const DEBOUNCE_MS: u64 = 300;
    const POLL_GAP_MS: u64 = 1000; // If no events for a while, confirm via polling.
    const TICK_MS: u64 = 200; // Event poll cadence.

    // Track last stable key & announce changes.
    let mut last_announce: Option<WindowKey> = None;
    let mut candidate: Option<(WindowKey, Instant)> = None;
    let mut last_signal = Instant::now();

    // Seed with current active window.
    if let Some(win) = get_active_window(&conn, root, atom_net_active_window)? {
        if let Some(key) = build_key(&conn, win, atom_net_wm_name, atom_wm_name, atom_utf8_string, atom_net_wm_pid)? {
            candidate = Some((key.clone(), Instant::now()));
            // If you want immediate first emit, comment out debounce block below and print now.
        }
    }

    loop {
        // Drain any pending events quickly.
        while let Some(ev) = conn.poll_for_event()? {
            if let Some(new_key) = handle_event(
                &conn,
                &ev,
                root,
                atom_net_active_window,
                atom_net_wm_name,
                atom_wm_name,
                atom_utf8_string,
                atom_net_wm_pid,
            )? {
                candidate = Some((new_key, Instant::now()));
                last_signal = Instant::now();
            }
        }

        // Fallback: if no events for a bit, poll the active window and compare.
        if last_signal.elapsed() >= Duration::from_millis(POLL_GAP_MS) {
            last_signal = Instant::now();
            if let Some(win) = get_active_window(&conn, root, atom_net_active_window)? {
                let polled = build_key(&conn, win, atom_net_wm_name, atom_wm_name, atom_utf8_string, atom_net_wm_pid)?;
                if polled.is_some() {
                    candidate = Some((polled.unwrap(), Instant::now()));
                } else {
                    // Could not resolve => treat as None candidate.
                    candidate = Some((
                        WindowKey {
                            platform_id: String::new(),
                            app_id: String::new(),
                            title: String::new(),
                        },
                        Instant::now(),
                    ));
                }
            } else {
                // No active window (rare on X11), set None.
                candidate = Some((
                    WindowKey {
                        platform_id: String::new(),
                        app_id: String::new(),
                        title: String::new(),
                    },
                    Instant::now(),
                ));
            }
        }

        // Debounce: only announce if candidate stayed stable long enough.
        if let Some((ref key, since)) = candidate {
            if since.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                let normalized = if key.platform_id.is_empty() && key.app_id.is_empty() && key.title.is_empty() {
                    None
                } else {
                    Some(key.clone())
                };
                if normalized != last_announce {
                    // "Emit" focus event — here we just print, but you can send via channel.
                    let ev = FocusEvent { key: normalized.clone(), ts: Instant::now() };
                    match ev.key {
                        Some(ref k) => {
                            println!(
                                "[FOCUS] {:?} @ {:?}",
                                k,
                                std::time::SystemTime::now()
                            );
                        }
                        None => {
                            println!("[FOCUS] None @ {:?}", std::time::SystemTime::now());
                        }
                    }
                    last_announce = normalized;
                }
            }
        }

        // Sleep a tick to keep CPU low.
        thread::sleep(Duration::from_millis(TICK_MS));
    }
}

// === Helpers ===

fn intern(conn: &RustConnection, name: &[u8]) -> Result<Atom> {
    let cookie = conn.intern_atom(false, name)?;
    Ok(cookie.reply()?.atom)
}

fn handle_event(
    conn: &RustConnection,
    event: &Event,
    root: Window,
    atom_net_active_window: Atom,
    atom_net_wm_name: Atom,
    atom_wm_name: Atom,
    atom_utf8_string: Atom,
    atom_net_wm_pid: Atom,
) -> Result<Option<WindowKey>> {
    if let Event::PropertyNotify(ev) = event {
        if ev.window == root && ev.atom == atom_net_active_window {
            if let Some(win) = get_active_window(conn, root, atom_net_active_window)? {
                return build_key(conn, win, atom_net_wm_name, atom_wm_name, atom_utf8_string, atom_net_wm_pid);
            } else {
                return Ok(Some(WindowKey {
                    platform_id: String::new(),
                    app_id: String::new(),
                    title: String::new(),
                }));
            }
        }
    }
    Ok(None)
}

fn get_active_window(conn: &RustConnection, root: Window, atom_net_active_window: Atom) -> Result<Option<Window>> {
    let prop = conn.get_property(false, root, <Atom as From<_>>::from(atom_net_active_window), <Atom as From<_>>::from(AtomEnum::WINDOW), 0, 1)?;
    let reply = prop.reply()?;
    if reply.type_ == AtomEnum::WINDOW.into() && reply.format == 32 && reply.value_len == 1 {
        let xid = u32::from_ne_bytes(reply.value[0..4].try_into().unwrap());
        Ok(Some(Window::from(xid)))
    } else {
        Ok(None)
    }
}

fn build_key(
    conn: &RustConnection,
    win: Window,
    atom_net_wm_name: Atom,
    atom_wm_name: Atom,
    atom_utf8_string: Atom,
    atom_net_wm_pid: Atom,
) -> Result<Option<WindowKey>> {
    // Some WMs set _NET_ACTIVE_WINDOW to 0 during transitions.
    if win == Window::from(0u32) {
        return Ok(None);
    }

    let title = get_window_title(conn, win, atom_net_wm_name, atom_wm_name, atom_utf8_string).unwrap_or_else(|_| "".into());
    let pid = get_window_pid(conn, win, atom_net_wm_pid).unwrap_or(0);
    let app_id = resolve_app_id(pid);

    let key = WindowKey {
        platform_id: format!("xid:{:#x}", u32::from(win)),
        app_id,
        title,
    };
    Ok(Some(key))
}

fn get_window_pid(conn: &RustConnection, win: Window, atom_net_wm_pid: Atom) -> Result<u32> {
    let prop = conn.get_property(false, win, <Atom as From<_>>::from(atom_net_wm_pid), <Atom as From<_>>::from(AtomEnum::CARDINAL), 0, 1)?;
    let reply = prop.reply()?;
    if reply.type_ == AtomEnum::CARDINAL.into() && reply.format == 32 && reply.value_len == 1 {
        let pid = u32::from_ne_bytes(reply.value[0..4].try_into().unwrap());
        Ok(pid)
    } else {
        Err(anyhow!("_NET_WM_PID missing"))
    }
}

fn get_window_title(
    conn: &RustConnection,
    win: Window,
    atom_net_wm_name: Atom,
    atom_wm_name: Atom,
    atom_utf8_string: Atom,
) -> Result<String> {
    // Try _NET_WM_NAME (UTF8)
    if let Ok(prop) = conn.get_property(false, win, atom_net_wm_name, atom_utf8_string, 0, u32::MAX) {
        let reply = prop.reply()?;
        if reply.type_ == atom_utf8_string && reply.format == 8 {
            return Ok(String::from_utf8_lossy(&reply.value).into_owned());
        }
    }
    // Fallback: WM_NAME (could be STRING; try to interpret as UTF-8 or Latin-1)
    let prop = conn.get_property(false, win, <Atom as From<_>>::from(atom_wm_name), <Atom as From<_>>::from(AtomEnum::ANY), 0, u32::MAX)?;
    let reply = prop.reply()?;
    if reply.format == 8 {
        // Best-effort decode: try UTF-8 else Latin-1.
        let s = match String::from_utf8(reply.value.clone()) {
            Ok(s) => s,
            Err(_) => reply.value.iter().map(|&b| b as char).collect(),
        };
        return Ok(s);
    }
    Err(anyhow!("No title"))
}

fn resolve_app_id(pid: u32) -> String {
    if pid == 0 {
        return "unknown".into();
    }
    // Prefer exe path; fallback to /proc/<pid>/comm.
    let exe = PathBuf::from(format!("/proc/{pid}/exe"));
    if let Ok(path) = fs::read_link(&exe) {
        return path.as_os_str().as_bytes().to_vec().into_iter().map(|b| b as char).collect();
    }
    let comm_path = format!("/proc/{pid}/comm");
    if let Ok(bytes) = fs::read(comm_path) {
        // Strip trailing newline
        let mut s = String::from_utf8_lossy(&bytes).into_owned();
        if s.ends_with('\n') { s.pop(); }
        return s;
    }
    "unknown".into()
}