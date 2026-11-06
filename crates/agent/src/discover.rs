// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Finding the printers of this machine for the setup: the Windows spooler (`EnumPrintersW`) or
//! `lpstat -e` on CUPS. Never an error: a machine without printers, or without `lpstat`, simply
//! has an empty list (a network printer can still be added by hand).

use crate::Platform;
use crate::backend_windows;
use crate::config::expressible_name;
use crate::proc::{Cmd, Runner};

/// One printer the machine knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundPrinter {
    pub name: String,
    /// Windows: a connection made for one user only. The agent runs as SYSTEM and does not see it.
    pub per_user: bool,
    /// False when the name has a character `printers` in `agent.ini` cannot express.
    pub usable: bool,
}

/// The printers of this machine, sorted by name. Any failure is an empty list.
pub fn discover(platform: Platform, run: &Runner) -> Vec<FoundPrinter> {
    match platform {
        Platform::Windows => from_entries(backend_windows::enum_printers().unwrap_or_default()),
        Platform::Cups => {
            let mut cmd = Cmd::new(vec!["lpstat".into(), "-e".into()], 10);
            cmd.capture = true;
            match run(&cmd) {
                Ok(out) => from_entries(
                    parse_lpstat_e(&String::from_utf8_lossy(&out))
                        .into_iter()
                        .map(|n| (n, false))
                        .collect(),
                ),
                Err(_) => Vec::new(),
            }
        }
    }
}

/// `lpstat -e` prints one destination name per line.
pub fn parse_lpstat_e(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// `(name, per_user)` pairs to a sorted list without duplicate names.
pub fn from_entries(entries: Vec<(String, bool)>) -> Vec<FoundPrinter> {
    let mut found: Vec<FoundPrinter> = entries
        .into_iter()
        .map(|(name, per_user)| FoundPrinter {
            usable: expressible_name(&name),
            name,
            per_user,
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found.dedup_by(|b, a| a.name == b.name);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn lpstat_lines_become_names() {
        assert_eq!(
            parse_lpstat_e("Zebra\n\nBüro_Drucker\n"),
            vec!["Zebra", "Büro_Drucker"]
        );
        assert_eq!(parse_lpstat_e("Zebra\r\nHP\r\n"), vec!["Zebra", "HP"]);
    }

    #[test]
    fn entries_are_sorted_deduped_and_marked() {
        let found = from_entries(vec![
            ("b;c".into(), false),
            ("Büro Drucker".into(), true),
            ("Büro Drucker".into(), true),
        ]);
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0],
            FoundPrinter {
                name: "Büro Drucker".into(),
                per_user: true,
                usable: true
            }
        );
        assert!(!found[1].usable);
    }

    #[test]
    fn a_missing_lpstat_is_an_empty_list() {
        let run: Runner = Rc::new(|_| Err("lpstat: not found".into()));
        assert!(discover(Platform::Cups, &run).is_empty());
    }

    #[test]
    fn cups_asks_lpstat_and_keeps_umlauts() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        let run: Runner = Rc::new(move |c| {
            s.borrow_mut().push(c.clone());
            Ok("Zebra\nBüro_Drucker\n".as_bytes().to_vec())
        });
        let found = discover(Platform::Cups, &run);
        let names: Vec<_> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Büro_Drucker", "Zebra"]);
        assert!(found.iter().all(|f| !f.per_user && f.usable));
        let seen = seen.borrow();
        assert_eq!(seen[0].argv, ["lpstat", "-e"]);
        assert!(seen[0].capture);
        assert_eq!(seen[0].timeout, std::time::Duration::from_secs(10));
    }

    #[cfg(windows)]
    #[test]
    fn the_real_spooler_answers() {
        assert!(crate::backend_windows::enum_printers().is_ok());
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_there_is_no_spooler_to_ask() {
        assert_eq!(crate::backend_windows::enum_printers(), Ok(vec![]));
    }
}
