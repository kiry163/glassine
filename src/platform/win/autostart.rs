//! The Startup-folder shortcut that launches glassine at logon.
//!
//! `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\glassine.lnk` is
//! the only autostart mechanism that needs neither elevation nor a registry
//! write, and it is the one the shell itself lists in Task Manager's Startup
//! tab, so the user can turn it off without knowing the file exists.
//!
//! The shortcut names the config file: the shell starts an autostarted process
//! with the Startup folder as its working directory, so a relative `--config`
//! would resolve against the wrong directory. [`install_into`] therefore
//! absolutises the path before writing it into the shortcut's arguments.

use super::PlatformError;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{absolute, Path, PathBuf};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

/// The shortcut's file name inside the Startup folder.
pub const LINK_NAME: &str = "glassine.lnk";

/// The Startup folder: `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup`.
pub fn startup_dir() -> Result<PathBuf, PlatformError> {
    let appdata = std::env::var_os("APPDATA").ok_or_else(|| {
        PlatformError::new("APPDATA is not set, so the Startup folder is unknown")
    })?;
    Ok(PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup"))
}

/// Writes `glassine.lnk` into `dir`, launching this executable with `config`.
///
/// An existing shortcut is replaced: installing twice must leave the folder in
/// the same state as installing once, with the new paths.
pub fn install_into(dir: &Path, config: &Path) -> Result<(), PlatformError> {
    let config = absolute(config).map_err(|error| {
        PlatformError::new(format!("cannot make {} absolute: {error}", config.display()))
    })?;
    let exe = std::env::current_exe().map_err(|error| {
        PlatformError::new(format!("cannot locate the running executable: {error}"))
    })?;

    let _apartment = Apartment::enter()?;
    let link = create_link()?;
    set_target(&link, &exe, &config)?;
    save(&link, &dir.join(LINK_NAME))
}

/// Deletes `glassine.lnk` from `dir`.
///
/// A missing shortcut is the state this exists to reach, so it is success —
/// which also makes a second call, and an uninstall that was never installed,
/// report nothing.
pub fn uninstall_from(dir: &Path) -> Result<(), PlatformError> {
    let path = dir.join(LINK_NAME);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(PlatformError::new(format!("cannot remove {}: {error}", path.display())))
        }
    }
}

/// Keeps the thread's COM apartment initialised for as long as it lives.
///
/// `CoUninitialize` is called only when this guard's own `CoInitializeEx` is
/// what put the thread in an apartment, so a host that already initialised COM
/// — or one that runs this on an MTA thread — keeps its own balance.
struct Apartment {
    initialized: bool,
}

impl Apartment {
    fn enter() -> Result<Apartment, PlatformError> {
        // SAFETY: the reserved pointer is documented to be null, and the
        // matching CoUninitialize (if any) is issued once by `Drop` below.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            // Another library already picked a mode for this thread. The shell
            // link objects still work there, so proceed without owning a count.
            return Ok(Apartment { initialized: false });
        }
        if hr.is_err() {
            return Err(PlatformError::new(format!("CoInitializeEx failed: {hr:?}")));
        }
        // S_FALSE: the thread was already initialised by an earlier call, which
        // owns the matching CoUninitialize.
        Ok(Apartment { initialized: hr != S_FALSE })
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: `initialized` is true only when the CoInitializeEx in
            // `enter` returned S_OK, so this balances that single call.
            unsafe { CoUninitialize() };
        }
    }
}

/// An empty shortcut from the shell's own coclass.
fn create_link() -> Result<IShellLinkW, PlatformError> {
    // SAFETY: `ShellLink` is an in-process coclass, for which a null outer
    // unknown is the documented way to ask for a private instance. The caller
    // holds an `Apartment`, so COM is initialised for this thread.
    let link = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) };
    link.map_err(PlatformError::from)
}

/// Points `link` at `exe` and hands it `--config "<config>"`.
fn set_target(link: &IShellLinkW, exe: &Path, config: &Path) -> Result<(), PlatformError> {
    let arguments = format!("--config \"{}\"", config.display());
    let target = wide(exe.as_os_str());
    let arguments = wide(OsStr::new(&arguments));
    // SAFETY: `link` is a live shell link and both buffers are NUL-terminated
    // and outlive the calls; the shell copies each string before returning.
    unsafe {
        link.SetPath(PCWSTR(target.as_ptr()))?;
        link.SetArguments(PCWSTR(arguments.as_ptr()))?;
    }
    Ok(())
}

/// Serialises `link` to `path`, replacing whatever is there.
fn save(link: &IShellLinkW, path: &Path) -> Result<(), PlatformError> {
    let utf16 = wide(path.as_os_str());
    let persist: IPersistFile = link.cast()?;
    // SAFETY: `utf16` is NUL-terminated and outlives the call; `fremember =
    // true` is what stores the shortcut's own path inside it.
    unsafe { persist.Save(PCWSTR(utf16.as_ptr()), true)? };
    Ok(())
}

/// A NUL-terminated UTF-16 copy of `text`, which is what the shell's link API
/// wants in place of every `PCWSTR` parameter.
fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::STGM_READ;

    #[test]
    fn install_then_uninstall_round_trips_in_an_injected_directory() {
        // Never the real Startup folder: the test owns its own directory.
        let dir = std::env::temp_dir().join("glassine-plan-autostart");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the injected directory");

        let config = std::env::temp_dir().join("glassine-plan-autostart.toml");
        install_into(&dir, &config).expect("install");
        // A second install replaces the shortcut rather than failing on it.
        install_into(&dir, &config).expect("reinstall");

        let link = dir.join(LINK_NAME);
        assert!(link.exists(), "no shortcut at {}", link.display());

        let (target, arguments) = read_back(&link);
        assert_eq!(target, std::env::current_exe().expect("current exe"));

        let expected = config.display().to_string();
        assert!(
            arguments.contains(&expected),
            "arguments {arguments:?} do not mention {expected}"
        );
        assert!(
            arguments.starts_with("--config "),
            "arguments {arguments:?} do not pass --config"
        );

        uninstall_from(&dir).expect("uninstall");
        assert!(!link.exists(), "shortcut survived uninstall");
        uninstall_from(&dir).expect("uninstalling twice is success");
    }

    /// The target and arguments an instance would see, read back through the
    /// shell's own shortcut object rather than by parsing the file.
    fn read_back(path: &Path) -> (PathBuf, String) {
        let _apartment = Apartment::enter().expect("COM apartment");
        let link = create_link().expect("shell link");
        let utf16 = wide(path.as_os_str());
        let persist: IPersistFile = link.cast().expect("IPersistFile");
        // SAFETY: `utf16` is NUL-terminated and outlives the call, and `link` is
        // a live shortcut that `Load` fills from that file.
        unsafe { persist.Load(PCWSTR(utf16.as_ptr()), STGM_READ) }.expect("load the shortcut");

        let mut target = [0u16; 1024];
        // SAFETY: both buffers are writable and their full lengths are passed
        // with them, so the shell truncates rather than writing past the end; a
        // null find-data out-parameter and no flags are the documented defaults.
        unsafe {
            link.GetPath(&mut target, std::ptr::null_mut(), 0).expect("read the target");
            let mut arguments = [0u16; 1024];
            link.GetArguments(&mut arguments).expect("read the arguments");
            (PathBuf::from(text(&target)), text(&arguments))
        }
    }

    /// What a wide buffer holds, up to its NUL.
    fn text(buffer: &[u16]) -> String {
        let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }
}
