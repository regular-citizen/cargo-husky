use fs::File;
use io::{BufRead, Read, Write};
use path::{Path, PathBuf};
#[cfg(not(target_os = "windows"))]
use std::os;
use std::{env, fmt, fs, io, path};

enum Error {
    GitDirNotFound,
    Io(io::Error),
    OutDir(env::VarError),
    InvalidUserHooksDir(PathBuf),
    EmptyUserHook(PathBuf),
}

type Result<T> = std::result::Result<T, Error>;

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Error {
        Error::Io(error)
    }
}

impl From<env::VarError> for Error {
    fn from(error: env::VarError) -> Error {
        Error::OutDir(error)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let msg = match self {
            Error::GitDirNotFound => format!(
                ".git directory was not found in '{}' or its parent directories",
                env::var("OUT_DIR").unwrap_or_else(|_| "".to_string()),
            ),
            Error::Io(inner) => format!("IO error: {}", inner),
            Error::OutDir(env::VarError::NotPresent) => unreachable!(),
            Error::OutDir(env::VarError::NotUnicode(msg)) => msg.to_string_lossy().to_string(),
            Error::InvalidUserHooksDir(path) => {
                format!("User hooks directory is not found or empty: {:?}", path)
            }
            Error::EmptyUserHook(path) => format!("User hook script is empty: {:?}", path),
        };
        write!(f, "{}", msg)
    }
}

fn resolve_gitdir() -> Result<PathBuf> {
    let dir = env::var("OUT_DIR")?;
    let mut dir = PathBuf::from(dir);
    if !dir.has_root() {
        dir = fs::canonicalize(dir)?;
    }
    loop {
        let gitdir = dir.join(".git");
        if gitdir.is_dir() {
            return Ok(gitdir);
        }
        if gitdir.is_file() {
            let mut buf = String::new();
            File::open(gitdir)?.read_to_string(&mut buf)?;
            let newlines: &[_] = &['\n', '\r'];
            let gitdir = PathBuf::from(buf.trim_right_matches(newlines));
            if !gitdir.is_dir() {
                return Err(Error::GitDirNotFound);
            }
            return Ok(gitdir);
        }
        if !dir.pop() {
            return Err(Error::GitDirNotFound);
        }
    }
}

// This function returns true when
//   - the hook was generated by the same version of cargo-husky
//   - someone else had already put another hook script
// For safety, cargo-husky does nothing on case2 also.
fn hook_already_exists(hook: &Path) -> bool {
    let f = match File::open(hook) {
        Ok(f) => f,
        Err(..) => return false,
    };

    let ver_line = match io::BufReader::new(f).lines().nth(2) {
        None => return true, // Less than 2 lines. The hook script seemed to be generated by someone else
        Some(Err(..)) => return false, // Failed to read entry. Re-generate anyway
        Some(Ok(line)) => line,
    };

    if !ver_line.contains("This hook was set by cargo-husky") {
        // The hook script was generated by someone else.
        true
    } else {
        let ver_comment = format!(
            "This hook was set by cargo-husky v{}",
            env!("CARGO_PKG_VERSION")
        );
        ver_line.contains(&ver_comment)
    }
}

fn write_script<W: io::Write>(w: &mut W) -> Result<()> {
    macro_rules! raw_cmd {
        ($c:expr) => {
            concat!("\necho '+", $c, "'\n", $c)
        }
    }
    macro_rules! cmd {
        ($c:expr) => {
            if cfg!(feature = "run-for-all") {
                raw_cmd!(concat!($c, " --all"))
            } else {
                raw_cmd!($c)
            }
        };
        ($c:expr, $subflags:expr) => {
            if cfg!(feature = "run-for-all") {
                raw_cmd!(concat!($c, " --all -- ", $subflags))
            } else {
                raw_cmd!(concat!($c, " -- ", $subflags))
            }
        };
    }

    let script = {
        let mut s = String::new();
        if cfg!(feature = "run-cargo-test") {
            s += cmd!("cargo test");
        }
        if cfg!(feature = "run-cargo-check") {
            s += cmd!("cargo check");
        }
        if cfg!(feature = "run-cargo-clippy") {
            s += cmd!("cargo clippy", "-D warnings");
        }
        if cfg!(feature = "run-cargo-fmt") {
            s += cmd!("cargo fmt",  "--check");
        }
        s
    };

    writeln!(
        w,
        r#"#!/bin/sh
#
# This hook was set by cargo-husky v{}: {}
# Generated by script {}{}build.rs
# Output at {}
#

set -e
{}"#,
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_HOMEPAGE"),
        env!("CARGO_MANIFEST_DIR"),
        path::MAIN_SEPARATOR,
        env::var("OUT_DIR").unwrap_or_else(|_| "".to_string()),
        script
    )?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn create_executable_file(path: &Path) -> io::Result<File> {
    File::create(path)
}

#[cfg(not(target_os = "windows"))]
fn create_executable_file(path: &Path) -> io::Result<File> {
    use os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(path)
}

fn install_hook(hook: &str) -> Result<()> {
    let hook_path = {
        let mut p = resolve_gitdir()?;
        p.push("hooks");
        p.push(hook);
        p
    };
    if !hook_already_exists(&hook_path) {
        let mut f = create_executable_file(&hook_path)?;
        write_script(&mut f)?;
    }
    Ok(())
}

fn install_user_hook(src: &Path, dst: &Path) -> Result<()> {
    if hook_already_exists(dst) {
        return Ok(());
    }

    let mut lines = {
        let mut vec = vec![];
        for line in io::BufReader::new(File::open(src)?).lines() {
            vec.push(line?);
        }
        vec
    };

    if lines.is_empty() {
        return Err(Error::EmptyUserHook(src.to_owned()));
    }

    // Insert cargo-husky package version information as comment
    if !lines[0].starts_with("#!") {
        lines.insert(0, "#".to_string());
    }
    lines.insert(1, "#".to_string());
    lines.insert(
        2,
        format!(
            "# This hook was set by cargo-husky v{}: {}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_HOMEPAGE")
        ),
    );

    let dst_file_path = dst.join(src.file_name().unwrap());

    let mut f = io::BufWriter::new(create_executable_file(&dst_file_path)?);
    for line in lines {
        writeln!(f, "{}", line)?;
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn is_executable_file(entry: &fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(ft) => ft.is_file(),
        Err(..) => false,
    }
}

#[cfg(not(target_os = "windows"))]
fn is_executable_file(entry: &fs::DirEntry) -> bool {
    use os::unix::fs::PermissionsExt;

    let ft = match entry.file_type() {
        Ok(ft) => ft,
        Err(..) => return false,
    };
    if !ft.is_file() {
        return false;
    }
    let md = match entry.metadata() {
        Ok(md) => md,
        Err(..) => return false,
    };
    let mode = md.permissions().mode();
    mode & 0o555 == 0o555 // Check file is read and executable mode
}

fn install_user_hooks() -> Result<()> {
    let git_dir = resolve_gitdir()?;
    let user_hooks_dir = {
        let mut p = git_dir.clone();
        p.pop();
        p.push(".cargo-husky");
        p.push("hooks");
        p
    };

    if !user_hooks_dir.is_dir() {
        return Err(Error::InvalidUserHooksDir(user_hooks_dir));
    }

    let hook_paths = fs::read_dir(&user_hooks_dir)?
        .filter_map(|e| e.ok().filter(is_executable_file).map(|e| e.path()))
        .collect::<Vec<_>>();

    if hook_paths.is_empty() {
        return Err(Error::InvalidUserHooksDir(user_hooks_dir));
    }

    let hooks_dir = git_dir.join("hooks");
    for path in hook_paths {
        install_user_hook(&path, &hooks_dir)?;
    }

    Ok(())
}

fn install() -> Result<()> {
    if cfg!(feature = "user-hooks") {
        return install_user_hooks();
    }
    if cfg!(feature = "prepush-hook") {
        install_hook("pre-push")?;
    }
    if cfg!(feature = "precommit-hook") {
        install_hook("pre-commit")?;
    }
    if cfg!(feature = "postmerge-hook") {
        install_hook("post-merge")?;
    }
    Ok(())
}

fn main() -> Result<()> {
    match install() {
        Err(e @ Error::GitDirNotFound) => {
            // #2
            eprintln!("Warning: {:?}", e);
            Ok(())
        }
        otherwise => otherwise,
    }
}
