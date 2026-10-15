//! The helper's command line.
//!
//! ```text
//! menu --x X --y Y [--owner HWND] [--theme dark|light] [--extended] [--background] -- PATHS...
//! verbs [--extended] [--background] -- PATHS...
//! invoke --verb VERB [--owner HWND] -- PATHS...
//! openwith [--owner HWND] PATH
//! ```
//!
//! `--stdin` in place of `-- PATHS...` reads one path per line from standard input, for
//! selections too long for a command line (Windows caps one at 32,767 characters).

use crate::theme::Theme;

/// What the helper was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Show the context menu at a point on screen and carry out the chosen command. Without a
    /// theme, the menu follows Windows' app mode.
    Menu { at: (i32, i32), target: Target, owner: Option<isize>, theme: Option<Theme> },
    /// Print the menu's items as JSON without showing anything.
    Verbs { target: Target },
    /// Carry out a command by its canonical verb, without showing the menu.
    Invoke { verb: String, target: Target, owner: Option<isize> },
    /// Show Windows' Open With dialog for one file.
    OpenWith { path: String, owner: Option<isize> },
}

/// The items a menu is for.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub paths: Vec<String>,
    /// Include the commands Explorer shows only while Shift is held.
    pub extended: bool,
    /// The menu of the folder itself, as for a click on empty space in it, rather than the
    /// menu of the folder as an item. Takes exactly one path.
    pub background: bool,
}

pub const USAGE: &str = "usage:
  kestrel-shellhost menu --x X --y Y [--owner HWND] [--theme dark|light] [--extended] [--background] -- PATHS...
  kestrel-shellhost verbs [--extended] [--background] -- PATHS...
  kestrel-shellhost invoke --verb VERB [--owner HWND] -- PATHS...
  kestrel-shellhost openwith [--owner HWND] PATH
--stdin in place of -- PATHS... reads one path per line from standard input.";

/// Options seen before the paths.
#[derive(Default)]
struct Options {
    x: Option<i32>,
    y: Option<i32>,
    owner: Option<isize>,
    verb: Option<String>,
    theme: Option<Theme>,
    extended: bool,
    background: bool,
    stdin: bool,
}

/// Parses the arguments after the program name. `stdin` is read only if `--stdin` is given.
pub fn parse(
    args: impl IntoIterator<Item = String>,
    stdin: impl FnOnce() -> std::io::Result<String>,
) -> Result<Command, String> {
    let mut args = args.into_iter();
    let mode = args.next().ok_or("no mode given")?;
    let mut options = Options::default();
    let mut paths = Vec::new();

    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--" => {
                paths.extend(args.by_ref());
                break;
            }
            "--x" => options.x = Some(number(&value("--x")?)?),
            "--y" => options.y = Some(number(&value("--y")?)?),
            "--owner" => options.owner = Some(handle(&value("--owner")?)?).filter(|&hwnd| hwnd != 0),
            "--verb" => options.verb = Some(value("--verb")?),
            "--theme" => {
                let name = value("--theme")?;
                let theme = Theme::parse(&name).ok_or_else(|| format!("--theme is dark or light, not {name}"))?;
                options.theme = Some(theme);
            }
            "--extended" => options.extended = true,
            "--background" => options.background = true,
            "--stdin" => options.stdin = true,
            // `openwith PATH` names its one file without a separator.
            _ if mode == "openwith" && !arg.starts_with("--") => paths.push(arg),
            _ => return Err(format!("unknown option {arg}")),
        }
    }

    if options.stdin {
        if !paths.is_empty() {
            return Err("give paths either after -- or with --stdin, not both".into());
        }
        let text = stdin().map_err(|e| format!("couldn't read paths from standard input: {e}"))?;
        paths = text.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect();
    }
    let paths: Vec<String> = paths.into_iter().map(|path| path.replace('/', "\\")).collect();
    if paths.is_empty() {
        return Err("no paths given".into());
    }
    if options.background && paths.len() != 1 {
        return Err("--background takes exactly one folder".into());
    }
    let target =
        |options: &Options, paths| Target { paths, extended: options.extended, background: options.background };

    match mode.as_str() {
        "menu" => {
            let (Some(x), Some(y)) = (options.x, options.y) else {
                return Err("menu needs --x and --y".into());
            };
            let target = target(&options, paths);
            Ok(Command::Menu { at: (x, y), target, owner: options.owner, theme: options.theme })
        }
        "verbs" => Ok(Command::Verbs { target: target(&options, paths) }),
        "invoke" => {
            let verb = options.verb.clone().ok_or("invoke needs --verb")?;
            Ok(Command::Invoke { verb, target: target(&options, paths), owner: options.owner })
        }
        "openwith" => {
            let [path]: [String; 1] = paths.try_into().map_err(|_| "openwith takes exactly one file")?;
            Ok(Command::OpenWith { path, owner: options.owner })
        }
        _ => Err(format!("unknown mode {mode}")),
    }
}

fn number(text: &str) -> Result<i32, String> {
    text.parse().map_err(|_| format!("{text} is not a whole number"))
}

/// A window handle, in decimal or as `0x` hexadecimal.
fn handle(text: &str) -> Result<isize, String> {
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => isize::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| format!("{text} is not a window handle"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(line: &str) -> Result<Command, String> {
        parse(line.split(' ').map(str::to_owned), || Ok("C:\\a.txt\r\n\r\nC:\\b.txt\r\n".into()))
    }

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|&path| path.to_owned()).collect()
    }

    #[test]
    fn parses_menu() {
        let command = run("menu --x -20 --y 300 --owner 0x1A2B --theme dark --extended -- C:\\a.txt C:/b.txt").unwrap();
        assert_eq!(
            command,
            Command::Menu {
                at: (-20, 300),
                target: Target { paths: paths(&["C:\\a.txt", "C:\\b.txt"]), extended: true, background: false },
                owner: Some(0x1A2B),
                theme: Some(Theme::Dark),
            }
        );
    }

    #[test]
    fn a_zero_owner_means_none_and_the_theme_is_optional() {
        let Command::Menu { owner, theme, .. } = run("menu --x 1 --y 2 --owner 0 -- C:\\a").unwrap() else { panic!() };
        assert_eq!((owner, theme), (None, None));
        let Command::Menu { theme, .. } = run("menu --x 1 --y 2 --theme light -- C:\\a").unwrap() else { panic!() };
        assert_eq!(theme, Some(Theme::Light));
    }

    #[test]
    fn parses_verbs_invoke_and_openwith() {
        assert_eq!(
            run("verbs --background -- C:\\Users").unwrap(),
            Command::Verbs { target: Target { paths: paths(&["C:\\Users"]), extended: false, background: true } }
        );
        assert_eq!(
            run("invoke --verb copy -- C:\\a.txt").unwrap(),
            Command::Invoke {
                verb: "copy".into(),
                target: Target { paths: paths(&["C:\\a.txt"]), extended: false, background: false },
                owner: None,
            }
        );
        assert_eq!(run("openwith C:\\a.txt").unwrap(), Command::OpenWith { path: "C:\\a.txt".into(), owner: None });
        assert_eq!(
            run("openwith --owner 42 -- C:\\a.txt").unwrap(),
            Command::OpenWith { path: "C:\\a.txt".into(), owner: Some(42) }
        );
    }

    #[test]
    fn paths_after_the_separator_may_look_like_options() {
        let Command::Verbs { target } = run("verbs -- --x").unwrap() else { panic!() };
        assert_eq!(target.paths, paths(&["--x"]));
    }

    #[test]
    fn reads_paths_from_stdin() {
        let Command::Verbs { target } = run("verbs --stdin").unwrap() else { panic!() };
        assert_eq!(target.paths, paths(&["C:\\a.txt", "C:\\b.txt"]));
        assert!(run("verbs --stdin -- C:\\c.txt").is_err());
    }

    #[test]
    fn rejects_bad_command_lines() {
        for line in [
            "",
            "dance -- C:\\a",
            "menu -- C:\\a",
            "menu --x 1 -- C:\\a",
            "menu --x one --y 2 -- C:\\a",
            "verbs",
            "verbs --",
            "verbs --what -- C:\\a",
            "verbs --background -- C:\\a C:\\b",
            "invoke -- C:\\a",
            "invoke --verb",
            "openwith C:\\a C:\\b",
            "menu --x 1 --y 2 --owner window -- C:\\a",
            "menu --x 1 --y 2 --theme blue -- C:\\a",
        ] {
            assert!(run(line).is_err(), "{line:?} should be refused");
        }
    }
}
