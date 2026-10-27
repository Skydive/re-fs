//! Mounting: start the session, wait for the first index, then serve the tree with
//! the platform's backend (FUSE on Linux, WinFsp on Windows) until Ctrl-C or an
//! unmount. With `follow_usb`, mount each time the tablet's cable goes in.

#[cfg(unix)]
mod fuse;
#[cfg(windows)]
mod winfsp;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use re_proto::index::tree::TreeOptions;

use crate::cache::Cache;
use crate::session::{self, Writes};
use crate::transport::{self, Target};

pub struct Options {
    /// A directory (Linux), or a drive like `R:` or a folder (Windows).
    pub mountpoint: PathBuf,
    pub tree: TreeOptions,
    pub writes: Writes,
    pub verbose: bool,
    /// Mount while the USB cable is in, unmount when it's pulled, then wait again.
    pub follow_usb: bool,
}

/// How a mount ended.
enum Ended {
    /// Ctrl-C, a termination signal, or someone else unmounted it: stop.
    Stopped,
    /// The tablet was unplugged (with `follow_usb`): wait for it again.
    Unplugged,
}

/// The USB link must be gone this long before a `follow_usb` mount goes away, so a
/// tablet waking from sleep (which drops the link for a moment) doesn't unmount it.
const UNPLUGGED_FOR: Duration = Duration::from_secs(5);

pub async fn mount(targets: Vec<Target>, opts: Options, cache: Arc<Cache>) -> Result<()> {
    if !opts.follow_usb {
        mount_once(targets, &opts, cache, std::future::pending()).await?;
        return Ok(());
    }
    if !targets.iter().any(|t| matches!(t, Target::Usb { .. })) {
        bail!("--follow-usb needs --usb");
    }
    loop {
        if !transport::usb_present() {
            eprintln!("re-fs: waiting for the tablet's USB cable");
            tokio::select! {
                _ = async { while !transport::usb_present() { tokio::time::sleep(Duration::from_secs(1)).await } } => {}
                _ = stop_signal() => return Ok(()),
            }
        }
        let unplugged = async {
            let mut gone = Duration::ZERO;
            while gone < UNPLUGGED_FOR {
                tokio::time::sleep(Duration::from_secs(1)).await;
                gone = if transport::usb_present() { Duration::ZERO } else { gone + Duration::from_secs(1) };
            }
        };
        match mount_once(targets.clone(), &opts, cache.clone(), unplugged).await {
            Ok(Ended::Stopped) => return Ok(()),
            Ok(Ended::Unplugged) => eprintln!("re-fs: tablet unplugged; unmounted"),
            Err(e) => {
                // E.g. plugged in but the agent isn't answering yet: try again shortly.
                eprintln!("re-fs: {e:#}; retrying in 10 s");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {}
                    _ = stop_signal() => return Ok(()),
                }
            }
        }
    }
}

async fn mount_once(targets: Vec<Target>, opts: &Options, cache: Arc<Cache>, unplugged: impl std::future::Future<Output = ()>) -> Result<Ended> {
    let shared = session::Shared::new(&opts.tree, cache, opts.writes, opts.verbose);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let via = targets.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
    let session = AbortOnDrop(tokio::spawn(session::run(targets, shared.clone(), opts.tree.clone(), ready_tx)));
    match tokio::time::timeout(Duration::from_secs(60), ready_rx).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => return Err(e),
        Ok(Err(_)) => bail!("session ended before connecting"),
        Err(_) => bail!("{via}: no index after 60 s"),
    }
    let writable = opts.writes != Writes::ReadOnly;
    let count = shared.tree.load().nodes().count();
    let mountpoint = opts.mountpoint.display();

    #[cfg(unix)]
    let ended = {
        let fs = fuse::ReFs::new(Arc::clone(&shared), tokio::runtime::Handle::current());
        let session = fuser::Session::new(fs, &opts.mountpoint, &fuse::mount_options(writable))
            .with_context(|| format!("mounting on {mountpoint}"))?;
        *shared.notifier.lock().unwrap() = Some(Arc::new(fuse::FuseNotifier(session.notifier())));
        let bg = session.spawn()?;
        eprintln!("re-fs: {count} items mounted on {mountpoint} via {via} (Ctrl-C to unmount)");
        // The session thread ends when someone else unmounts (fusermount3 -u).
        let unmounted = async {
            while !bg.guard.is_finished() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        };
        let ended = tokio::select! {
            _ = stop_signal() => Ended::Stopped,
            _ = unmounted => {
                eprintln!("re-fs: unmounted");
                Ended::Stopped
            }
            _ = unplugged => Ended::Unplugged,
        };
        drop(bg); // unmounts, if still mounted
        ended
    };

    #[cfg(windows)]
    let ended = {
        let mounted = winfsp::Mounted::start(Arc::clone(&shared), tokio::runtime::Handle::current(), &opts.mountpoint, writable)
            .with_context(|| format!("mounting on {mountpoint}"))?;
        eprintln!("re-fs: {count} items mounted on {} via {via} (Ctrl-C to unmount)", mounted.mount_point());
        let ended = tokio::select! {
            _ = stop_signal() => Ended::Stopped,
            _ = unplugged => Ended::Unplugged,
        };
        *shared.notifier.lock().unwrap() = None;
        drop(mounted); // unmounts
        ended
    };

    drop(session);
    Ok(ended)
}

/// Ctrl-C, or the OS asking us to stop (SIGTERM; closing the console window on Windows).
async fn stop_signal() {
    #[cfg(unix)]
    {
        let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(windows)]
    {
        let Ok(mut close) = tokio::signal::windows::ctrl_close() else { return std::future::pending().await };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = close.recv() => {}
        }
    }
}

/// The session task stops with the mount (it would otherwise keep the link open).
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
