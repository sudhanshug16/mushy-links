use anyhow::{Context, Result, bail, ensure};
use clap::{Subcommand, ValueEnum};
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    #[value(alias = "pwsh")]
    Powershell,
}

#[derive(Subcommand)]
pub enum ShellCommand {
    /// Install ln -m and make mushy available in new terminals and SSH sessions.
    Install {
        shell: Option<Shell>,
        /// Use a custom profile instead of the default shell startup files.
        #[arg(long)]
        rc_file: Option<PathBuf>,
    },
    /// Print the shell setup for immediate use or a custom configuration.
    Env { shell: Shell },
}

pub fn run(command: &ShellCommand) -> Result<()> {
    let exe = std::env::current_exe()?;
    let bin = exe
        .parent()
        .context("cannot find mushy's install directory")?;
    match command {
        ShellCommand::Env { shell } => {
            print!("{}{}", path_code(*shell, bin)?, function_code(*shell))
        }
        ShellCommand::Install { shell, rc_file } => {
            let shell = shell.map(Ok).unwrap_or_else(detect)?;
            let home = directories::UserDirs::new()
                .context("cannot find your home directory")?
                .home_dir()
                .to_owned();
            let files = if let Some(path) = rc_file {
                vec![(path.clone(), true, true)]
            } else {
                profiles(shell, &home)?
            };
            for (file, path, function) in files {
                update_profile(
                    &file,
                    path.then(|| path_code(shell, bin)).transpose()?.as_deref(),
                    function.then(|| function_code(shell)),
                    matches!(shell, Shell::Powershell),
                )?;
                println!("Updated {}", file.display());
            }
            println!("Open a new terminal, then use: ln -m HOST:/path ./local");
            println!("Ordinary ln commands keep using your system's ln.");
        }
    }
    Ok(())
}

fn detect() -> Result<Shell> {
    let name = std::env::var("MUSHY_SHELL")
        .ok()
        .or_else(|| std::env::var("SHELL").ok());
    let name = name
        .as_deref()
        .and_then(|s| Path::new(s).file_name())
        .and_then(|s| s.to_str())
        .unwrap_or(if cfg!(windows) { "powershell" } else { "bash" });
    Shell::from_str(name.trim_end_matches(".exe"), true).map_err(|_| {
        anyhow::anyhow!("shell {name} is unsupported; choose bash, zsh, fish, or powershell")
    })
}

fn profiles(shell: Shell, home: &Path) -> Result<Vec<(PathBuf, bool, bool)>> {
    Ok(match shell {
        Shell::Bash => {
            let login = [".bash_profile", ".bash_login", ".profile"]
                .into_iter()
                .map(|name| home.join(name))
                .find(|p| p.exists())
                .unwrap_or_else(|| home.join(".profile"));
            vec![(home.join(".bashrc"), true, true), (login, true, true)]
        }
        Shell::Zsh => {
            let dir = std::env::var_os("ZDOTDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.to_owned());
            vec![
                (dir.join(".zshenv"), true, false),
                (dir.join(".zshrc"), false, true),
            ]
        }
        Shell::Fish => {
            let dir = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            vec![(dir.join("fish/config.fish"), true, true)]
        }
        Shell::Powershell => {
            let user =
                directories::UserDirs::new().context("cannot find your Documents directory")?;
            let documents = user
                .document_dir()
                .context("cannot find your Documents directory; use --rc-file")?;
            vec![(documents.join("PowerShell/profile.ps1"), true, true)]
        }
    })
}

fn quote(text: &str, shell: Shell) -> String {
    match shell {
        Shell::Powershell => format!("'{}'", text.replace('\'', "''")),
        Shell::Fish => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
        _ => format!("'{}'", text.replace('\'', "'\\''")),
    }
}

fn path_code(shell: Shell, bin: &Path) -> Result<String> {
    let path = bin
        .to_str()
        .context("install directory must use UTF-8")?
        .to_owned();
    #[cfg(windows)]
    let path = if matches!(shell, Shell::Bash | Shell::Zsh | Shell::Fish) {
        let output = std::process::Command::new("cygpath")
            .arg("-u")
            .arg(bin)
            .output()
            .context("Git Bash requires cygpath to set up the shell")?;
        ensure!(
            output.status.success(),
            "cannot convert the install directory for Git Bash"
        );
        String::from_utf8(output.stdout)?.trim_end().to_owned()
    } else {
        path
    };
    let path = quote(&path, shell);
    Ok(match shell {
        Shell::Bash | Shell::Zsh => format!(
            "case \":$PATH:\" in\n    *:{path}:*) ;;\n    *) export PATH={path}:\"$PATH\" ;;\nesac\n"
        ),
        Shell::Fish => {
            format!("if not contains -- {path} $PATH\n    set -gx PATH {path} $PATH\nend\n")
        }
        Shell::Powershell => {
            let separator = if cfg!(windows) { ';' } else { ':' };
            format!(
                "if (($env:PATH -split '{separator}') -notcontains {path}) {{ $env:PATH = {path} + '{separator}' + $env:PATH }}\n"
            )
        }
    })
}

fn function_code(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash | Shell::Zsh => {
            r#"unalias ln 2>/dev/null || true
ln() {
    local mushy_arg
    for mushy_arg in "$@"; do
        case "$mushy_arg" in
            --) break ;;
            --*) continue ;;
            -*m*) command mushy ln -- "$@"; return $? ;;
        esac
    done
    command ln "$@"
}
"#
        }
        Shell::Fish => {
            r#"function ln
    for mushy_arg in $argv
        if test "$mushy_arg" = --
            break
        end
        if string match -rq '^-[^-]*m' -- "$mushy_arg"
            command mushy ln -- $argv
            return $status
        end
    end
    command ln $argv
end
"#
        }
        Shell::Powershell => {
            r#"Remove-Item Alias:ln -Force -ErrorAction SilentlyContinue
function global:ln {
    $mushyArgs = @($args)
    foreach ($mushyArg in $mushyArgs) {
        if ($mushyArg -eq '--') { break }
        if ($mushyArg -match '^-[^-]*m') { & mushy ln '--' @mushyArgs; return }
    }
    $nativeLn = Get-Command ln -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($nativeLn) { & $nativeLn.Source @mushyArgs }
    else { throw 'No native ln command is installed. Use New-Item -ItemType SymbolicLink for a local link.' }
}
"#
        }
    }
}

const PATH_START: &str = "# >>> mushy PATH >>>";
const PATH_END: &str = "# <<< mushy PATH <<<";
const LN_START: &str = "# >>> mushy ln >>>";
const LN_END: &str = "# <<< mushy ln <<<";

fn managed_content(original: &str, path: Option<&str>, function: Option<&str>) -> Result<String> {
    let mut retained = String::new();
    let mut end = None;
    for line in original.split_inclusive('\n') {
        let marker = line.trim_end_matches(['\r', '\n']);
        match (end, marker) {
            (None, PATH_START) => end = Some(PATH_END),
            (None, LN_START) => end = Some(LN_END),
            (Some(expected), found) if expected == found => end = None,
            (Some(_), PATH_START | LN_START) | (None, PATH_END | LN_END) => {
                bail!("malformed mushy block; profile left untouched")
            }
            (Some(_), _) => (),
            (None, _) => retained.push_str(line),
        }
    }
    ensure!(
        end.is_none(),
        "unfinished mushy block; profile left untouched"
    );
    let mut result = String::new();
    if let Some(path) = path {
        result.push_str(&format!("{PATH_START}\n{path}{PATH_END}\n"));
    }
    result.push_str(&retained);
    if let Some(function) = function {
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(&format!("{LN_START}\n{function}{LN_END}\n"));
    }
    Ok(result)
}

#[derive(Clone, Copy)]
enum ProfileEncoding {
    Utf8(bool),
    Utf16(bool),
}

fn decode_profile(bytes: &[u8], powershell: bool) -> Result<(String, ProfileEncoding)> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        ensure!(
            powershell,
            "UTF-16 profiles are supported only for PowerShell"
        );
        ensure!(
            bytes.len().is_multiple_of(2),
            "invalid UTF-16 profile; file left untouched"
        );
        let big = bytes[0] == 0xfe;
        let units: Vec<_> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| {
                if big {
                    u16::from_be_bytes([b[0], b[1]])
                } else {
                    u16::from_le_bytes([b[0], b[1]])
                }
            })
            .collect();
        Ok((
            String::from_utf16(&units).context("invalid UTF-16 profile")?,
            ProfileEncoding::Utf16(big),
        ))
    } else {
        let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
        let text = if bom { &bytes[3..] } else { bytes };
        Ok((
            String::from_utf8(text.to_vec()).context("profile must use UTF-8 or UTF-16")?,
            ProfileEncoding::Utf8(bom || powershell),
        ))
    }
}

fn encode_profile(text: &str, encoding: ProfileEncoding) -> Vec<u8> {
    let mut result = Vec::new();
    match encoding {
        ProfileEncoding::Utf8(bom) => {
            if bom {
                result.extend_from_slice(&[0xef, 0xbb, 0xbf]);
            }
            result.extend_from_slice(text.as_bytes());
        }
        ProfileEncoding::Utf16(big) => {
            result.extend_from_slice(if big { &[0xfe, 0xff] } else { &[0xff, 0xfe] });
            for unit in text.encode_utf16() {
                result.extend_from_slice(&if big {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                });
            }
        }
    }
    result
}

fn update_profile(
    path: &Path,
    code: Option<&str>,
    function: Option<&str>,
    powershell: bool,
) -> Result<()> {
    let path = if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        path.canonicalize()
            .context("cannot resolve the shell profile symlink")?
    } else {
        path.to_owned()
    };
    let original = match fs::read(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).context("cannot read the shell profile"),
    };
    let (text, encoding) = decode_profile(original.as_deref().unwrap_or_default(), powershell)?;
    let updated = encode_profile(&managed_content(&text, code, function)?, encoding);
    if original.as_deref() == Some(&updated) {
        return Ok(());
    }
    let parent = path.parent().context("profile needs a parent directory")?;
    fs::create_dir_all(parent)?;
    let metadata = fs::metadata(&path).ok();
    if let Some(original) = &original {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".mushy.bak");
        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(backup)
        {
            Ok(mut backup) => {
                if let Some(meta) = &metadata {
                    backup.set_permissions(meta.permissions())?;
                }
                backup.write_all(original)?;
                backup.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e).context("cannot back up the shell profile"),
        }
    }
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    if let Some(meta) = metadata {
        stage.as_file().set_permissions(meta.permissions())?;
    }
    stage.write_all(&updated)?;
    stage.as_file().sync_all()?;
    stage
        .persist(&path)
        .context("cannot update the shell profile")?;
    Ok(())
}

pub fn link_args(args: Vec<OsString>) -> Result<Vec<OsString>> {
    let mut result = vec![OsString::from("mushy"), OsString::from("link")];
    let mut options = true;
    let mut remote = false;
    for arg in args {
        if options && arg == "--" {
            options = false;
        } else if options
            && let Some(flag) = arg.to_str().and_then(|s| s.strip_prefix('-'))
            && !flag.starts_with('-')
            && !flag.is_empty()
            && flag.chars().any(|c| c == 'm' || c == 's')
        {
            ensure!(
                flag.chars().all(|c| c == 'm' || c == 's'),
                "ln -m supports -m and optional -s; use mushy link for other options"
            );
            remote |= flag.contains('m');
            continue;
        }
        result.push(arg);
    }
    ensure!(remote, "use ln -m HOST:/path LOCAL to create a remote link");
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_updates_preserve_content_and_are_idempotent() {
        let original = "# user config\ncase $- in *i*) ;; *) return;; esac\nalias ln='ln -v'\n";
        let updated = managed_content(
            original,
            Some("export PATH='/bin':\"$PATH\"\n"),
            Some(function_code(Shell::Bash)),
        )
        .unwrap();
        assert!(updated.contains(original));
        assert!(updated.find("export PATH").unwrap() < updated.find("return;;").unwrap());
        assert!(updated.find("unalias ln").unwrap() > updated.find("alias ln='ln -v'").unwrap());
        assert_eq!(
            managed_content(
                &updated,
                Some("export PATH='/bin':\"$PATH\"\n"),
                Some(function_code(Shell::Bash))
            )
            .unwrap(),
            updated
        );
        assert!(managed_content("# >>> mushy PATH >>>\nuser config", None, None).is_err());
    }
    #[test]
    fn profile_is_backed_up_before_editing() {
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join("profile");
        fs::write(&rc, "# original\n").unwrap();
        update_profile(&rc, Some("PATH=test\n"), None, false).unwrap();
        update_profile(&rc, Some("PATH=updated\n"), None, false).unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("profile.mushy.bak")).unwrap(),
            "# original\n"
        );
        assert!(fs::read_to_string(rc).unwrap().contains("# original\n"));
    }
    #[test]
    fn powershell_profiles_preserve_utf16_and_back_up_original_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join("profile.ps1");
        let original = encode_profile("# café\r\n", ProfileEncoding::Utf16(false));
        fs::write(&rc, &original).unwrap();
        update_profile(
            &rc,
            Some("# new setup\n"),
            Some(function_code(Shell::Powershell)),
            true,
        )
        .unwrap();
        let saved = fs::read(&rc).unwrap();
        assert!(saved.starts_with(&[0xff, 0xfe]));
        assert!(
            decode_profile(&saved, true)
                .unwrap()
                .0
                .contains("# café\r\n")
        );
        assert_eq!(
            fs::read(tmp.path().join("profile.ps1.mushy.bak")).unwrap(),
            original
        );
        update_profile(
            &rc,
            Some("# new setup\n"),
            Some(function_code(Shell::Powershell)),
            true,
        )
        .unwrap();
        assert_eq!(fs::read(&rc).unwrap(), saved);
    }
    #[cfg(unix)]
    #[test]
    fn profile_symlinks_remain_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("dotfiles");
        let rc = tmp.path().join("profile");
        fs::write(&target, "# original\n").unwrap();
        std::os::unix::fs::symlink(&target, &rc).unwrap();
        update_profile(&rc, Some("PATH=test\n"), None, false).unwrap();
        assert!(fs::symlink_metadata(&rc).unwrap().file_type().is_symlink());
        assert!(fs::read_to_string(target).unwrap().contains("# original\n"));
    }
    #[test]
    fn ln_arguments_preserve_spaces_and_double_dash() {
        let args = ["-ms", "--", "box:/a b", "-my local"]
            .map(OsString::from)
            .to_vec();
        assert_eq!(
            link_args(args).unwrap(),
            ["mushy", "link", "--", "box:/a b", "-my local"].map(OsString::from)
        );
        assert!(link_args(vec!["-mf".into(), "box:/a".into(), "local".into()]).is_err());
        assert!(link_args(vec!["--".into(), "-m".into()]).is_err());
    }
}
